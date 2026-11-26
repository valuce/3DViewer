//! Format-neutral scene representation produced by all loaders.

use glam::{Mat3, Mat4, Vec3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topology {
    Triangles,
    Lines,
    Points,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque,
    Mask,
    Blend,
}

#[derive(Clone, Debug)]
pub struct Mesh {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    /// Same length as positions after `Scene::finalize`.
    pub normals: Vec<[f32; 3]>,
    /// Empty or same length as positions.
    pub uvs: Vec<[f32; 2]>,
    /// Empty or same length as positions.
    pub colors: Vec<[u8; 4]>,
    /// Triangle list / line list indices. Empty for point clouds (draw all vertices).
    pub indices: Vec<u32>,
    pub topology: Topology,
    pub material: Option<usize>,
    /// True when the source file supplied normals (false = generated).
    pub had_normals: bool,
    /// Animated scenes: node this mesh is attached to (positions are in node space).
    pub node: Option<usize>,
    /// Animated scenes: skin index (positions are in bind space).
    pub skin: Option<usize>,
    /// Up to four joint indices per vertex (into the skin's joint list).
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub morph_targets: Vec<MorphTarget>,
    /// Default morph weights (used when no animation drives them).
    pub morph_weights: Vec<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct MorphTarget {
    /// Position deltas, same length as the mesh positions.
    pub positions: Vec<[f32; 3]>,
    /// Normal deltas (may be empty).
    pub normals: Vec<[f32; 3]>,
}

impl Mesh {
    pub fn new(name: impl Into<String>, topology: Topology) -> Self {
        Self {
            name: name.into(),
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            colors: Vec::new(),
            indices: Vec::new(),
            topology,
            material: None,
            had_normals: false,
            node: None,
            skin: None,
            joints: Vec::new(),
            weights: Vec::new(),
            morph_targets: Vec::new(),
            morph_weights: Vec::new(),
        }
    }

    pub fn triangle_count(&self) -> usize {
        match self.topology {
            Topology::Triangles => self.indices.len() / 3,
            _ => 0,
        }
    }

    /// Apply a transform to positions and normals (normals via inverse-transpose).
    pub fn transform(&mut self, m: &Mat4) {
        if *m == Mat4::IDENTITY {
            return;
        }
        for p in &mut self.positions {
            *p = m.transform_point3(Vec3::from(*p)).to_array();
        }
        let nm = Mat3::from_mat4(*m).inverse().transpose();
        for n in &mut self.normals {
            *n = (nm * Vec3::from(*n)).normalize_or_zero().to_array();
        }
        if m.determinant() < 0.0 && self.topology == Topology::Triangles {
            for t in self.indices.chunks_exact_mut(3) {
                t.swap(1, 2);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Material {
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub base_color_tex: Option<usize>,
    pub normal_tex: Option<usize>,
    pub mr_tex: Option<usize>,
    pub occlusion_tex: Option<usize>,
    pub emissive_tex: Option<usize>,
    pub double_sided: bool,
    pub alpha_mode: AlphaMode,
    pub alpha_cutoff: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: "default".into(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            emissive: [0.0; 3],
            base_color_tex: None,
            normal_tex: None,
            mr_tex: None,
            occlusion_tex: None,
            emissive_tex: None,
            double_sided: false,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextureData {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, width*height*4 bytes.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn empty() -> Self {
        Self { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) }
    }
    pub fn extend(&mut self, p: Vec3) {
        if p.is_finite() {
            self.min = self.min.min(p);
            self.max = self.max.max(p);
        }
    }
    pub fn is_valid(&self) -> bool {
        self.min.x <= self.max.x && self.min.y <= self.max.y && self.min.z <= self.max.z
    }
    pub fn center(&self) -> Vec3 {
        if self.is_valid() { (self.min + self.max) * 0.5 } else { Vec3::ZERO }
    }
    pub fn size(&self) -> Vec3 {
        if self.is_valid() { self.max - self.min } else { Vec3::ZERO }
    }
    /// Bounding sphere radius; never zero so cameras can frame single points.
    pub fn radius(&self) -> f32 {
        let r = self.size().length() * 0.5;
        if r.is_finite() && r > 1e-6 { r } else { 1.0 }
    }

    /// Distance from the origin to the box (0 when the origin is inside it).
    pub fn distance_to_origin(&self) -> f32 {
        if self.is_valid() { Vec3::ZERO.clamp(self.min, self.max).length() } else { 0.0 }
    }

    /// The file's origin is the asset's pivot. It counts as near when it lies within two
    /// model radii of the box; a far origin (scans, CAD parts in assembly coordinates)
    /// would shrink the model to a dot if the view tried to include it.
    pub fn pivot_near(&self) -> bool {
        self.is_valid() && self.distance_to_origin() <= self.radius() * 2.0
    }

    /// What the camera frames and the grid covers: the model plus its pivot when near.
    pub fn view_bounds(&self) -> Aabb {
        let mut b = *self;
        if self.pivot_near() {
            b.extend(Vec3::ZERO);
        }
        b
    }

    /// Where the origin sits in the box per axis: 0 = min side, 0.5 = middle, 1 = max side
    /// (outside the 0..1 range when the origin is outside the box).
    pub fn pivot_fraction(&self) -> Vec3 {
        let eps = self.radius() * 1e-6;
        let f = |min: f32, size: f32| {
            if size > eps {
                -min / size
            } else if min.abs() <= eps {
                0.5
            } else if min > 0.0 {
                -1.0
            } else {
                2.0
            }
        };
        let s = self.size();
        Vec3::new(f(self.min.x, s.x), f(self.min.y, s.y), f(self.min.z, s.z))
    }
}

#[derive(Clone, Debug, Default)]
pub struct SceneInfo {
    pub format: String,
    pub file_size: u64,
    pub load_ms: f64,
    pub nodes: usize,
    pub animations: Vec<String>,
    pub skins: usize,
    pub bones: usize,
    pub cameras: usize,
    pub lights: usize,
    pub morph_targets: usize,
    /// Model authored Z-up (STL, 3MF, PLY usually).
    pub up_axis_z: bool,
    pub warnings: Vec<String>,
    /// Extra key/value facts (generator, version, units ...).
    pub extra: Vec<(String, String)>,
    /// Metres per model unit when the file says (glTF, FBX, 3MF, DAE, BLEND).
    pub unit_m: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub textures: Vec<TextureData>,
    pub info: SceneInfo,
    /// Present when the file has animations, skins or morph targets.
    pub anim: Option<crate::anim::AnimData>,
}

impl Scene {
    pub fn bounds(&self) -> Aabb {
        if let Some(a) = &self.anim {
            return crate::anim::rest_bounds(&self.meshes, a);
        }
        let mut b = Aabb::empty();
        for m in &self.meshes {
            for p in &m.positions {
                b.extend(Vec3::from(*p));
            }
        }
        b
    }

    /// Fill in missing normals, validate attribute lengths and indices.
    pub fn finalize(&mut self) {
        let nmat = self.materials.len();
        let mut warnings = Vec::new();
        for m in &mut self.meshes {
            let n = m.positions.len();
            if m.uvs.len() != n {
                m.uvs.clear();
            }
            if m.colors.len() != n {
                m.colors.clear();
            }
            if m.material.is_some_and(|i| i >= nmat) {
                m.material = None;
            }
            if m.joints.len() != n || m.weights.len() != n {
                m.joints.clear();
                m.weights.clear();
            }
            m.morph_targets.retain(|t| t.positions.len() == n);
            for t in &mut m.morph_targets {
                if t.normals.len() != n {
                    t.normals.clear();
                }
            }
            let bad = m.indices.iter().any(|&i| i as usize >= n);
            if bad {
                warnings.push(format!("mesh '{}': out of range indices dropped", m.name));
                let per = if m.topology == Topology::Lines { 2 } else { 3 };
                let old = std::mem::take(&mut m.indices);
                m.indices = old.chunks_exact(per).filter(|c| c.iter().all(|&i| (i as usize) < n)).flatten().copied().collect();
            }
            if m.normals.len() != n {
                m.had_normals = false;
                m.normals = match m.topology {
                    Topology::Triangles => compute_normals(&m.positions, &m.indices),
                    _ => vec![[0.0, 0.0, 0.0]; n],
                };
            }
        }
        self.meshes.retain(|m| !m.positions.is_empty());
        self.info.warnings.extend(warnings);
        if let Some(a) = &mut self.anim {
            a.finalize();
            let ns = a.skins.len();
            let nn = a.nodes.len();
            for m in &mut self.meshes {
                if m.skin.is_some_and(|s| s >= ns) {
                    m.skin = None;
                }
                if m.node.is_some_and(|x| x >= nn) {
                    m.node = None;
                }
            }
        }
    }

    pub fn vertex_count(&self) -> usize {
        self.meshes.iter().map(|m| m.positions.len()).sum()
    }
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.triangle_count()).sum()
    }
}

/// Area-weighted smooth vertex normals for an indexed triangle list.
pub fn compute_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![Vec3::ZERO; positions.len()];
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if a >= positions.len() || b >= positions.len() || c >= positions.len() {
            continue;
        }
        let pa = Vec3::from(positions[a]);
        let n = (Vec3::from(positions[b]) - pa).cross(Vec3::from(positions[c]) - pa);
        acc[a] += n;
        acc[b] += n;
        acc[c] += n;
    }
    acc.into_iter()
        .map(|n| {
            let n = n.normalize_or_zero();
            if n == Vec3::ZERO { [0.0, 0.0, 1.0] } else { n.to_array() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_normals_point_up_z() {
        let p = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
        let n = compute_normals(&p, &[0, 1, 2, 0, 2, 3]);
        for v in n {
            assert!((Vec3::from(v) - Vec3::Z).length() < 1e-6);
        }
    }

    #[test]
    fn empty_aabb_invalid_and_point_radius_finite() {
        assert!(!Aabb::empty().is_valid());
        let mut b = Aabb::empty();
        b.extend(Vec3::new(1.0, 2.0, 3.0));
        assert!(b.is_valid());
        assert_eq!(b.center(), Vec3::new(1.0, 2.0, 3.0));
        assert!(b.radius().is_finite() && b.radius() > 0.0);
    }

    fn boxed(min: [f32; 3], max: [f32; 3]) -> Aabb {
        Aabb { min: Vec3::from(min), max: Vec3::from(max) }
    }

    #[test]
    fn pivot_at_bottom_centre() {
        let b = boxed([-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]);
        assert!(b.pivot_near());
        assert_eq!(b.pivot_fraction(), Vec3::new(0.5, 0.0, 0.5));
        assert_eq!(b.view_bounds(), b);
    }

    #[test]
    fn offset_pivot_is_kept_in_view() {
        // Pivot at a corner, model sits beside it.
        let b = boxed([1.0, 0.0, 1.0], [3.0, 2.0, 3.0]);
        assert!(b.pivot_near());
        assert_eq!(b.view_bounds(), boxed([0.0, 0.0, 0.0], [3.0, 2.0, 3.0]));
        let f = b.pivot_fraction();
        assert!(f.x < 0.0 && f.z < 0.0 && f.y == 0.0);
    }

    #[test]
    fn far_pivot_is_left_out() {
        let b = boxed([1000.0, 0.0, 1000.0], [1001.0, 1.0, 1001.0]);
        assert!(!b.pivot_near());
        assert_eq!(b.view_bounds(), b);
        assert!(b.distance_to_origin() > 1000.0);
    }

    #[test]
    fn flat_box_pivot_fraction() {
        let b = boxed([-1.0, 0.0, -1.0], [1.0, 0.0, 1.0]);
        assert_eq!(b.pivot_fraction(), Vec3::new(0.5, 0.5, 0.5));
    }

    #[test]
    fn finalize_generates_normals_and_drops_bad_indices() {
        let mut s = Scene::default();
        let mut m = Mesh::new("t", Topology::Triangles);
        m.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        m.indices = vec![0, 1, 2, 0, 1, 9];
        s.meshes.push(m);
        s.finalize();
        assert_eq!(s.meshes[0].indices, vec![0, 1, 2]);
        assert_eq!(s.meshes[0].normals.len(), 3);
        assert!(!s.meshes[0].had_normals);
        assert_eq!(s.info.warnings.len(), 1);
    }

    #[test]
    fn mirrored_transform_flips_winding() {
        let mut m = Mesh::new("t", Topology::Triangles);
        m.positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        m.indices = vec![0, 1, 2];
        m.transform(&Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0)));
        assert_eq!(m.indices, vec![0, 2, 1]);
    }
}
