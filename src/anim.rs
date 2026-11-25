//! Skeletal / node / morph animation: data model, clip sampling and CPU deformation.

use crate::scene::{Aabb, Mesh};
use glam::{Mat3, Mat4, Quat, Vec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trs {
    pub t: Vec3,
    pub r: Quat,
    pub s: Vec3,
}

impl Default for Trs {
    fn default() -> Self {
        Self { t: Vec3::ZERO, r: Quat::IDENTITY, s: Vec3::ONE }
    }
}

impl Trs {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.s, self.r, self.t)
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    pub rest: Trs,
    /// Fixed matrix applied after the TRS (used when a rest transform cannot be decomposed).
    pub post: Option<Mat4>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    Step,
    Linear,
    /// glTF cubic spline: values are stored as (in-tangent, value, out-tangent) triplets.
    Cubic,
}

#[derive(Clone, Debug)]
pub enum TrackValues {
    Translation(Vec<Vec3>),
    Rotation(Vec<Quat>),
    Scale(Vec<Vec3>),
    /// Flattened `times × targets` morph weights.
    Weights(Vec<f32>),
}

#[derive(Clone, Debug)]
pub struct Track {
    pub node: usize,
    pub interp: Interp,
    pub times: Vec<f32>,
    pub values: TrackValues,
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug)]
pub struct Skin {
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
}

#[derive(Clone, Debug, Default)]
pub struct AnimData {
    pub nodes: Vec<Node>,
    pub clips: Vec<Clip>,
    pub skins: Vec<Skin>,
    /// Parents before children; filled by `finalize`.
    order: Vec<usize>,
}

impl AnimData {
    /// Validate references and compute the evaluation order. Must be called once after building.
    pub fn finalize(&mut self) {
        let n = self.nodes.len();
        for node in &mut self.nodes {
            if node.parent.is_some_and(|p| p >= n) {
                node.parent = None;
            }
        }
        // Break cycles defensively, then order parents first.
        let mut state = vec![0u8; n]; // 0 new, 1 visiting, 2 done
        let mut order = Vec::with_capacity(n);
        for start in 0..n {
            let mut chain = Vec::new();
            let mut cur = Some(start);
            while let Some(c) = cur {
                if state[c] == 2 {
                    break;
                }
                if state[c] == 1 {
                    self.nodes[c].parent = None; // cycle
                    break;
                }
                state[c] = 1;
                chain.push(c);
                cur = self.nodes[c].parent;
            }
            for &c in chain.iter().rev() {
                state[c] = 2;
                order.push(c);
            }
        }
        self.order = order;
        for clip in &mut self.clips {
            clip.tracks.retain(|t| t.node < n && !t.times.is_empty());
        }
        for skin in &mut self.skins {
            skin.joints.retain(|&j| j < n);
            skin.inverse_bind.resize(skin.joints.len(), Mat4::IDENTITY);
        }
    }

    pub fn has_motion(&self) -> bool {
        self.clips.iter().any(|c| !c.tracks.is_empty())
    }

    /// True when no two clips animate the same node (exporters often split one
    /// timeline into per-object clips, which should then play together).
    pub fn clips_disjoint(&self) -> bool {
        let mut owner = vec![usize::MAX; self.nodes.len()];
        for (ci, c) in self.clips.iter().enumerate() {
            for t in &c.tracks {
                match owner.get(t.node) {
                    Some(&o) if o != usize::MAX && o != ci => return false,
                    Some(_) => owner[t.node] = ci,
                    None => {}
                }
            }
        }
        true
    }
}

/// Evaluated pose: world matrix and morph weights per node.
pub struct Pose {
    pub worlds: Vec<Mat4>,
    pub weights: Vec<Option<Vec<f32>>>,
}

fn find_key(times: &[f32], t: f32) -> (usize, usize, f32) {
    let n = times.len();
    if n == 1 || t <= times[0] {
        return (0, 0, 0.0);
    }
    if t >= times[n - 1] {
        return (n - 1, n - 1, 0.0);
    }
    let i = times.partition_point(|&x| x <= t).saturating_sub(1).min(n - 2);
    let span = times[i + 1] - times[i];
    let f = if span > 0.0 { (t - times[i]) / span } else { 0.0 };
    (i, i + 1, f)
}

fn hermite<T>(p0: T, m0: T, p1: T, m1: T, f: f32, dt: f32) -> T
where
    T: std::ops::Mul<f32, Output = T> + std::ops::Add<Output = T> + Copy,
{
    let f2 = f * f;
    let f3 = f2 * f;
    p0 * (2.0 * f3 - 3.0 * f2 + 1.0) + m0 * ((f3 - 2.0 * f2 + f) * dt) + p1 * (-2.0 * f3 + 3.0 * f2) + m1 * ((f3 - f2) * dt)
}

fn sample_vec3(track: &Track, v: &[Vec3], t: f32) -> Vec3 {
    let (a, b, f) = find_key(&track.times, t);
    match track.interp {
        Interp::Cubic => {
            let get = |k: usize, j: usize| v.get(k * 3 + j).copied().unwrap_or(Vec3::ZERO);
            if a == b {
                return get(a, 1);
            }
            let dt = track.times[b] - track.times[a];
            hermite(get(a, 1), get(a, 2), get(b, 1), get(b, 0), f, dt)
        }
        Interp::Step => v.get(a).copied().unwrap_or(Vec3::ZERO),
        Interp::Linear => {
            let (va, vb) = (v.get(a).copied().unwrap_or(Vec3::ZERO), v.get(b).copied().unwrap_or(Vec3::ZERO));
            va.lerp(vb, f)
        }
    }
}

fn sample_quat(track: &Track, v: &[Quat], t: f32) -> Quat {
    let (a, b, f) = find_key(&track.times, t);
    let q = match track.interp {
        Interp::Cubic => {
            let get = |k: usize, j: usize| v.get(k * 3 + j).copied().unwrap_or(Quat::IDENTITY);
            if a == b {
                get(a, 1)
            } else {
                let dt = track.times[b] - track.times[a];
                let h = |p0: Quat, m0: Quat, p1: Quat, m1: Quat| {
                    let f2 = f * f;
                    let f3 = f2 * f;
                    let c = |x: Quat, k: f32| Quat::from_xyzw(x.x * k, x.y * k, x.z * k, x.w * k);
                    let add = |x: Quat, y: Quat| Quat::from_xyzw(x.x + y.x, x.y + y.y, x.z + y.z, x.w + y.w);
                    add(
                        add(c(p0, 2.0 * f3 - 3.0 * f2 + 1.0), c(m0, (f3 - 2.0 * f2 + f) * dt)),
                        add(c(p1, -2.0 * f3 + 3.0 * f2), c(m1, (f3 - f2) * dt)),
                    )
                };
                h(get(a, 1), get(a, 2), get(b, 1), get(b, 0))
            }
        }
        Interp::Step => v.get(a).copied().unwrap_or(Quat::IDENTITY),
        Interp::Linear => {
            let (qa, qb) = (v.get(a).copied().unwrap_or(Quat::IDENTITY), v.get(b).copied().unwrap_or(Quat::IDENTITY));
            qa.slerp(qb, f)
        }
    };
    let len = q.length();
    if len > 1e-8 && len.is_finite() { q / len } else { Quat::IDENTITY }
}

fn sample_weights(track: &Track, v: &[f32], t: f32) -> Vec<f32> {
    let keys = track.times.len();
    let per = match track.interp {
        Interp::Cubic => v.len() / (keys * 3).max(1),
        _ => v.len() / keys.max(1),
    };
    let (a, b, f) = find_key(&track.times, t);
    (0..per)
        .map(|i| match track.interp {
            Interp::Cubic => {
                let get = |k: usize, j: usize| v.get((k * 3 + j) * per + i).copied().unwrap_or(0.0);
                if a == b {
                    get(a, 1)
                } else {
                    hermite(get(a, 1), get(a, 2), get(b, 1), get(b, 0), f, track.times[b] - track.times[a])
                }
            }
            Interp::Step => v.get(a * per + i).copied().unwrap_or(0.0),
            Interp::Linear => {
                let x = v.get(a * per + i).copied().unwrap_or(0.0);
                let y = v.get(b * per + i).copied().unwrap_or(0.0);
                x + (y - x) * f
            }
        })
        .collect()
}

/// Evaluate the given clips at time `t` (empty slice = rest pose). Later clips win on conflicts.
pub fn evaluate(anim: &AnimData, clips: &[usize], t: f32) -> Pose {
    let n = anim.nodes.len();
    let mut locals: Vec<Trs> = anim.nodes.iter().map(|n| n.rest).collect();
    let mut weights: Vec<Option<Vec<f32>>> = vec![None; n];
    for c in clips.iter().filter_map(|&c| anim.clips.get(c)) {
        for tr in &c.tracks {
            let l = &mut locals[tr.node];
            match &tr.values {
                TrackValues::Translation(v) => l.t = sample_vec3(tr, v, t),
                TrackValues::Scale(v) => l.s = sample_vec3(tr, v, t),
                TrackValues::Rotation(v) => l.r = sample_quat(tr, v, t),
                TrackValues::Weights(v) => weights[tr.node] = Some(sample_weights(tr, v, t)),
            }
        }
    }
    let mut worlds = vec![Mat4::IDENTITY; n];
    for &i in &anim.order {
        let node = &anim.nodes[i];
        let mut local = locals[i].matrix();
        if let Some(p) = node.post {
            local *= p;
        }
        worlds[i] = match node.parent {
            Some(p) => worlds[p] * local,
            None => local,
        };
    }
    Pose { worlds, weights }
}

/// Model matrix for a mesh in a pose: skinned meshes ignore their node transform.
pub fn model_matrix(mesh: &Mesh, pose: &Pose) -> Mat4 {
    if mesh.skin.is_some() {
        return Mat4::IDENTITY;
    }
    mesh.node.and_then(|n| pose.worlds.get(n)).copied().unwrap_or(Mat4::IDENTITY)
}

pub fn needs_deform(mesh: &Mesh) -> bool {
    (mesh.skin.is_some() && !mesh.joints.is_empty()) || !mesh.morph_targets.is_empty()
}

/// Apply morph targets and skinning. Output positions/normals are in model space
/// (world space for skinned meshes).
pub fn deform(mesh: &Mesh, anim: &AnimData, pose: &Pose, pos: &mut Vec<[f32; 3]>, nrm: &mut Vec<[f32; 3]>) {
    pos.clear();
    nrm.clear();
    pos.extend_from_slice(&mesh.positions);
    nrm.extend_from_slice(&mesh.normals);
    nrm.resize(pos.len(), [0.0; 3]);

    if !mesh.morph_targets.is_empty() {
        let w: &[f32] = mesh.node.and_then(|n| pose.weights.get(n)).and_then(|w| w.as_deref()).unwrap_or(&mesh.morph_weights);
        for (ti, target) in mesh.morph_targets.iter().enumerate() {
            let k = w.get(ti).copied().unwrap_or(0.0);
            if k.abs() < 1e-6 {
                continue;
            }
            for (p, d) in pos.iter_mut().zip(&target.positions) {
                p[0] += d[0] * k;
                p[1] += d[1] * k;
                p[2] += d[2] * k;
            }
            for (p, d) in nrm.iter_mut().zip(&target.normals) {
                p[0] += d[0] * k;
                p[1] += d[1] * k;
                p[2] += d[2] * k;
            }
        }
    }

    let Some(skin) = mesh.skin.and_then(|s| anim.skins.get(s)) else { return };
    if mesh.joints.len() != pos.len() || mesh.weights.len() != pos.len() {
        return;
    }
    let mats: Vec<Mat4> = skin.joints.iter().zip(&skin.inverse_bind).map(|(&j, ib)| pose.worlds[j] * *ib).collect();
    if mats.is_empty() {
        return;
    }
    let deform_one = |p: &mut [f32; 3], n: &mut [f32; 3], j: &[u16; 4], w: &[f32; 4]| {
        let mut m = Mat4::ZERO;
        let mut total = 0.0;
        for k in 0..4 {
            if w[k] > 0.0 {
                if let Some(jm) = mats.get(j[k] as usize) {
                    m += *jm * w[k];
                    total += w[k];
                }
            }
        }
        if total <= 1e-6 {
            return;
        }
        if (total - 1.0).abs() > 1e-3 {
            m *= 1.0 / total;
        }
        *p = m.transform_point3(Vec3::from(*p)).to_array();
        *n = (Mat3::from_mat4(m) * Vec3::from(*n)).normalize_or_zero().to_array();
    };
    let work = pos.len();
    let threads = if work > 200_000 { std::thread::available_parallelism().map(|c| c.get()).unwrap_or(4).min(8) } else { 1 };
    if threads <= 1 {
        for i in 0..work {
            deform_one(&mut pos[i], &mut nrm[i], &mesh.joints[i], &mesh.weights[i]);
        }
    } else {
        let chunk = work.div_ceil(threads);
        std::thread::scope(|s| {
            for ((pc, nc), start) in pos.chunks_mut(chunk).zip(nrm.chunks_mut(chunk)).zip((0..).step_by(chunk)) {
                let deform_one = &deform_one;
                s.spawn(move || {
                    for (k, (p, n)) in pc.iter_mut().zip(nc.iter_mut()).enumerate() {
                        let i = start + k;
                        deform_one(p, n, &mesh.joints[i], &mesh.weights[i]);
                    }
                });
            }
        });
    }
}

/// Bounds of all meshes in the rest pose.
pub fn rest_bounds(meshes: &[Mesh], anim: &AnimData) -> Aabb {
    let pose = evaluate(anim, &[], 0.0);
    let mut b = Aabb::empty();
    let (mut p, mut n) = (Vec::new(), Vec::new());
    for m in meshes {
        let model = model_matrix(m, &pose);
        let src: &[[f32; 3]] = if needs_deform(m) {
            deform(m, anim, &pose, &mut p, &mut n);
            &p
        } else {
            &m.positions
        };
        for v in src {
            b.extend(model.transform_point3(Vec3::from(*v)));
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Topology;

    fn two_nodes() -> AnimData {
        let mut a = AnimData {
            nodes: vec![
                Node { name: "root".into(), parent: None, rest: Trs { t: Vec3::new(1.0, 0.0, 0.0), ..Default::default() }, post: None },
                Node { name: "child".into(), parent: Some(0), rest: Trs { t: Vec3::new(0.0, 2.0, 0.0), ..Default::default() }, post: None },
            ],
            clips: vec![Clip {
                name: "move".into(),
                duration: 1.0,
                tracks: vec![Track {
                    node: 0,
                    interp: Interp::Linear,
                    times: vec![0.0, 1.0],
                    values: TrackValues::Translation(vec![Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0)]),
                }],
            }],
            skins: vec![],
            order: vec![],
        };
        a.finalize();
        a
    }

    #[test]
    fn hierarchy_and_linear_sampling() {
        let a = two_nodes();
        let rest = evaluate(&a, &[], 0.0);
        assert_eq!(rest.worlds[1].transform_point3(Vec3::ZERO), Vec3::new(1.0, 2.0, 0.0));
        let mid = evaluate(&a, &[0], 0.5);
        assert!((mid.worlds[1].transform_point3(Vec3::ZERO) - Vec3::new(5.0, 2.0, 0.0)).length() < 1e-5);
        let after = evaluate(&a, &[0], 5.0); // clamps to last key
        assert!((after.worlds[0].transform_point3(Vec3::ZERO).x - 10.0).abs() < 1e-5);
    }

    #[test]
    fn children_before_parents_still_evaluate() {
        let mut a = AnimData {
            nodes: vec![
                Node { name: "child".into(), parent: Some(1), rest: Trs { t: Vec3::X, ..Default::default() }, post: None },
                Node { name: "root".into(), parent: None, rest: Trs { t: Vec3::Y, ..Default::default() }, post: None },
            ],
            ..Default::default()
        };
        a.finalize();
        let p = evaluate(&a, &[], 0.0);
        assert_eq!(p.worlds[0].transform_point3(Vec3::ZERO), Vec3::new(1.0, 1.0, 0.0));
    }

    #[test]
    fn cycle_does_not_hang() {
        let mut a = AnimData {
            nodes: vec![
                Node { name: "a".into(), parent: Some(1), rest: Trs::default(), post: None },
                Node { name: "b".into(), parent: Some(0), rest: Trs::default(), post: None },
            ],
            ..Default::default()
        };
        a.finalize();
        let p = evaluate(&a, &[], 0.0);
        assert!(p.worlds.iter().all(|m| m.is_finite()));
    }

    #[test]
    fn skinning_follows_joint() {
        let mut a = two_nodes();
        a.skins.push(Skin { joints: vec![0], inverse_bind: vec![Mat4::from_translation(Vec3::new(-1.0, 0.0, 0.0))] });
        a.finalize();
        let mut m = Mesh::new("s", Topology::Triangles);
        m.positions = vec![[1.0, 0.0, 0.0]];
        m.normals = vec![[0.0, 1.0, 0.0]];
        m.skin = Some(0);
        m.joints = vec![[0, 0, 0, 0]];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]];
        let pose = evaluate(&a, &[0], 1.0); // root moved to x = 10
        let (mut p, mut n) = (Vec::new(), Vec::new());
        deform(&m, &a, &pose, &mut p, &mut n);
        assert!((Vec3::from(p[0]) - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn cubic_spline_hits_keys() {
        let tr = Track {
            node: 0,
            interp: Interp::Cubic,
            times: vec![0.0, 1.0],
            values: TrackValues::Translation(vec![Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::splat(4.0), Vec3::ZERO]),
        };
        let TrackValues::Translation(v) = &tr.values else { unreachable!() };
        assert_eq!(sample_vec3(&tr, v, 0.0), Vec3::ZERO);
        assert_eq!(sample_vec3(&tr, v, 1.0), Vec3::splat(4.0));
        let mid = sample_vec3(&tr, v, 0.5);
        assert!((mid - Vec3::splat(2.0)).length() < 1e-5);
    }
}
