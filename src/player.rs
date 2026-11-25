//! Animation playback: keeps the data needed to pose an animated scene on the GPU.

use crate::anim::{self, AnimData};
use crate::render::Renderer;
use crate::render::gpu_scene::Vertex;
use crate::scene::{Mesh, Scene};
use glam::Mat4;

struct Item {
    gpu: usize,
    node: Option<usize>,
    skinned: bool,
    /// CPU copy for meshes that need skinning / morphing every frame.
    deform: Option<Mesh>,
}

pub struct Player {
    pub data: AnimData,
    items: Vec<Item>,
    /// Selected clip; None = all clips together.
    pub clip: Option<usize>,
    pub time: f32,
    pub playing: bool,
    pub speed: f32,
    pub looping: bool,
    dirty: bool,
    p: Vec<[f32; 3]>,
    n: Vec<[f32; 3]>,
    verts: Vec<Vertex>,
}

impl Player {
    /// Build a player for a scene uploaded in the same mesh order. None for static scenes.
    pub fn new(scene: &Scene) -> Option<Self> {
        let data = scene.anim.clone()?;
        let items = scene
            .meshes
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let deform = anim::needs_deform(m);
                Item { gpu: i, node: m.node, skinned: m.skin.is_some() && deform, deform: deform.then(|| m.clone()) }
            })
            .collect();
        let all = data.clips.len() > 1 && data.clips_disjoint();
        let first = (!data.clips.is_empty() && !all).then_some(0);
        let playing = data.has_motion();
        Some(Self {
            data,
            items,
            clip: first,
            time: 0.0,
            playing,
            speed: 1.0,
            looping: true,
            dirty: true,
            p: Vec::new(),
            n: Vec::new(),
            verts: Vec::new(),
        })
    }

    pub fn duration(&self) -> f32 {
        match self.clip {
            Some(c) => self.data.clips.get(c).map(|c| c.duration).unwrap_or(0.0),
            None => self.data.clips.iter().map(|c| c.duration).fold(0.0, f32::max),
        }
    }

    fn active(&self) -> Vec<usize> {
        match self.clip {
            Some(c) => vec![c],
            None => (0..self.data.clips.len()).collect(),
        }
    }

    pub fn set_clip(&mut self, clip: Option<usize>) {
        self.clip = clip;
        self.time = 0.0;
        self.dirty = true;
    }

    pub fn seek(&mut self, t: f32) {
        self.time = t.clamp(0.0, self.duration());
        self.dirty = true;
    }

    pub fn toggle(&mut self) {
        if !self.playing && self.time >= self.duration() && !self.looping {
            self.time = 0.0;
        }
        self.playing = !self.playing;
    }

    /// Advance time; returns true while a repaint is needed.
    pub fn tick(&mut self, dt: f32) -> bool {
        let d = self.duration();
        if self.playing && d > 0.0 {
            self.time += dt * self.speed;
            if self.time > d {
                if self.looping {
                    self.time %= d;
                } else {
                    self.time = d;
                    self.playing = false;
                }
            }
            self.dirty = true;
        }
        self.playing
    }

    /// Push the current pose to the GPU if anything changed.
    pub fn apply(&mut self, r: &mut Renderer) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let pose = anim::evaluate(&self.data, &self.active(), self.time);
        for it in &self.items {
            let model = if it.skinned { Mat4::IDENTITY } else { it.node.and_then(|n| pose.worlds.get(n)).copied().unwrap_or(Mat4::IDENTITY) };
            r.set_mesh_model(it.gpu, model);
            if let Some(m) = &it.deform {
                anim::deform(m, &self.data, &pose, &mut self.p, &mut self.n);
                self.verts.clear();
                self.verts.extend((0..self.p.len()).map(|k| Vertex {
                    pos: self.p[k],
                    normal: self.n[k],
                    uv: m.uvs.get(k).copied().unwrap_or([0.0; 2]),
                    color: m.colors.get(k).copied().unwrap_or([255; 4]),
                }));
                r.update_mesh_vertices(it.gpu, &self.verts);
            }
        }
    }
}
