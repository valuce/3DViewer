//! Offscreen scene renderer. Draws into an MSAA target that egui shows as an image.

pub mod gpu_scene;
pub mod mipmap;

use crate::scene::{Scene, Topology};
use crate::settings::DisplayMode;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use gpu_scene::{DefaultTextures, GpuScene, UploadCtx, vertex_layout};
use mipmap::MipGen;
use std::collections::HashMap;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const EGUI_VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameUniform {
    view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    light_dir: [f32; 4],
    up: [f32; 4],
    wire_color: [f32; 4],
    viewport: [f32; 4],
    bg_top: [f32; 4],
    bg_bottom: [f32; 4],
    mode: u32,
    flags: u32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// Everything the renderer needs to draw one frame.
pub struct FrameParams {
    pub view_proj: Mat4,
    pub eye: Vec3,
    /// Direction from target to eye (used for orthographic shading).
    pub back_dir: Vec3,
    pub ortho: bool,
    pub light_dir: Vec3,
    pub light_intensity: f32,
    pub up: Vec3,
    pub mode: DisplayMode,
    pub textures: bool,
    pub flat: bool,
    pub vertex_colors: bool,
    pub cull: bool,
    pub wire_overlay: bool,
    pub point_size: f32,
    pub bg_top: [u8; 3],
    pub bg_bottom: [u8; 3],
    pub overlay: Vec<LineVertex>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum PKind {
    Fill { cull: bool, blend: bool },
    DepthOnly { cull: bool },
    Wire,
    LineMesh,
    Points,
    Overlay,
    Background,
}

struct Targets {
    size: [u32; 2],
    msaa: Option<wgpu::TextureView>,
    depth: wgpu::TextureView,
    resolve: wgpu::TextureView,
    /// Kept alive because egui samples it.
    _egui_view: wgpu::TextureView,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    module: wgpu::ShaderModule,
    material_bgl: wgpu::BindGroupLayout,
    mesh_bgl: wgpu::BindGroupLayout,
    mesh_layout: wgpu::PipelineLayout,
    overlay_layout: wgpu::PipelineLayout,
    frame_buf: wgpu::Buffer,
    frame_bg: wgpu::BindGroup,
    pipelines: HashMap<PKind, wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    defaults: DefaultTextures,
    mipgen: MipGen,
    targets: Option<Targets>,
    egui_id: Option<egui::TextureId>,
    overlay_buf: Option<(wgpu::Buffer, u64)>,
    pub sample_count: u32,
    pub wire_supported: bool,
    pub scene: Option<GpuScene>,
    pub adapter_info: String,
}

impl Renderer {
    pub fn new(rs: &egui_wgpu::RenderState) -> Self {
        let device = rs.device.clone();
        let queue = rs.queue.clone();
        let info = rs.adapter.get_info();
        let adapter_info = format!("{} ({:?})", info.name, info.backend);
        let wire_supported = device.features().contains(wgpu::Features::POLYGON_MODE_LINE);
        let fmt_features = rs.adapter.get_texture_format_features(COLOR_FORMAT);
        let sample_count = if fmt_features.flags.sample_count_supported(4) { 4 } else { 1 };

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let uniform_entry = |binding: u32, dynamic: bool, size: u64| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: wgpu::BufferSize::new(size),
            },
            count: None,
        };
        let frame_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame bgl"),
            entries: &[uniform_entry(0, false, std::mem::size_of::<FrameUniform>() as u64)],
        });
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let material_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material bgl"),
            entries: &[
                uniform_entry(0, false, std::mem::size_of::<gpu_scene::MaterialUniform>() as u64),
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                tex_entry(4),
                tex_entry(5),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let mesh_bgl = device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("mesh bgl"), entries: &[uniform_entry(0, true, gpu_scene::MESH_UNIFORM_SIZE)] });
        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh layout"),
            bind_group_layouts: &[Some(&frame_bgl), Some(&material_bgl), Some(&mesh_bgl)],
            immediate_size: 0,
        });
        let overlay_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay layout"),
            bind_group_layouts: &[Some(&frame_bgl)],
            immediate_size: 0,
        });
        let frame_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame ubo"),
            size: std::mem::size_of::<FrameUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame bg"),
            layout: &frame_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: frame_buf.as_entire_binding() }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        let defaults = DefaultTextures::new(&device, &queue);
        let mipgen = MipGen::new(&device);
        let mut r = Self {
            device,
            queue,
            module,
            material_bgl,
            mesh_bgl,
            mesh_layout,
            overlay_layout,
            frame_buf,
            frame_bg,
            pipelines: HashMap::new(),
            sampler,
            defaults,
            mipgen,
            targets: None,
            egui_id: None,
            overlay_buf: None,
            sample_count,
            wire_supported,
            scene: None,
            adapter_info,
        };
        // Pipelines needed for the empty view are built up front.
        r.pipeline(PKind::Background);
        r.pipeline(PKind::Overlay);
        r
    }

    pub fn upload(&mut self, scene: &Scene) {
        self.scene = None; // free old GPU memory first
        let mut ctx = UploadCtx {
            device: &self.device,
            queue: &self.queue,
            material_bgl: &self.material_bgl,
            mesh_bgl: &self.mesh_bgl,
            sampler: &self.sampler,
            defaults: &self.defaults,
            mipgen: &mut self.mipgen,
        };
        self.scene = Some(gpu_scene::upload(&mut ctx, scene));
    }

    pub fn clear_scene(&mut self) {
        self.scene = None;
    }

    pub fn set_mesh_model(&mut self, index: usize, model: Mat4) {
        if let Some(s) = self.scene.as_mut() {
            s.set_model(&self.queue, index, model);
        }
    }

    pub fn update_mesh_vertices(&mut self, index: usize, verts: &[gpu_scene::Vertex]) {
        if let Some(s) = self.scene.as_mut() {
            s.update_vertices(&self.queue, index, verts);
        }
    }

    pub fn set_highlight(&mut self, index: Option<usize>) {
        if let Some(s) = self.scene.as_mut() {
            s.set_highlight(&self.queue, index);
        }
    }

    /// Build pipelines that a display mode needs before they are first used.
    pub fn warm_up(&mut self) {
        for k in
            [PKind::Fill { cull: false, blend: false }, PKind::Fill { cull: true, blend: false }, PKind::Fill { cull: false, blend: true }]
        {
            self.pipeline(k);
        }
    }

    fn pipeline(&mut self, kind: PKind) -> wgpu::RenderPipeline {
        if let Some(p) = self.pipelines.get(&kind) {
            return p.clone();
        }
        let p = self.create_pipeline(kind);
        self.pipelines.insert(kind, p.clone());
        p
    }

    fn create_pipeline(&self, kind: PKind) -> wgpu::RenderPipeline {
        let blend_alpha = Some(wgpu::BlendState::ALPHA_BLENDING);
        let (vs, fs, layout, topology, polygon, cull, depth_write, compare, blend, bias, write_mask) = match kind {
            PKind::Fill { cull, blend } => (
                "vs_main",
                Some("fs_main"),
                &self.mesh_layout,
                wgpu::PrimitiveTopology::TriangleList,
                wgpu::PolygonMode::Fill,
                cull && !blend,
                !blend,
                wgpu::CompareFunction::LessEqual,
                if blend { blend_alpha } else { None },
                true,
                wgpu::ColorWrites::ALL,
            ),
            PKind::DepthOnly { cull } => (
                "vs_main",
                Some("fs_wire"),
                &self.mesh_layout,
                wgpu::PrimitiveTopology::TriangleList,
                wgpu::PolygonMode::Fill,
                cull,
                true,
                wgpu::CompareFunction::LessEqual,
                None,
                true,
                wgpu::ColorWrites::empty(),
            ),
            PKind::Wire => (
                "vs_main",
                Some("fs_wire"),
                &self.mesh_layout,
                wgpu::PrimitiveTopology::TriangleList,
                wgpu::PolygonMode::Line,
                false,
                false,
                wgpu::CompareFunction::LessEqual,
                blend_alpha,
                false,
                wgpu::ColorWrites::ALL,
            ),
            PKind::LineMesh => (
                "vs_main",
                Some("fs_line_mesh"),
                &self.mesh_layout,
                wgpu::PrimitiveTopology::LineList,
                wgpu::PolygonMode::Fill,
                false,
                true,
                wgpu::CompareFunction::LessEqual,
                None,
                false,
                wgpu::ColorWrites::ALL,
            ),
            PKind::Points => (
                "vs_points",
                Some("fs_points"),
                &self.mesh_layout,
                wgpu::PrimitiveTopology::TriangleList,
                wgpu::PolygonMode::Fill,
                false,
                true,
                wgpu::CompareFunction::LessEqual,
                None,
                false,
                wgpu::ColorWrites::ALL,
            ),
            PKind::Overlay => (
                "vs_overlay",
                Some("fs_overlay"),
                &self.overlay_layout,
                wgpu::PrimitiveTopology::LineList,
                wgpu::PolygonMode::Fill,
                false,
                false,
                wgpu::CompareFunction::LessEqual,
                blend_alpha,
                false,
                wgpu::ColorWrites::ALL,
            ),
            PKind::Background => (
                "vs_bg",
                Some("fs_bg"),
                &self.overlay_layout,
                wgpu::PrimitiveTopology::TriangleList,
                wgpu::PolygonMode::Fill,
                false,
                false,
                wgpu::CompareFunction::Always,
                None,
                false,
                wgpu::ColorWrites::ALL,
            ),
        };
        let overlay_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        let buffers: Vec<Option<wgpu::VertexBufferLayout>> = match kind {
            PKind::Background => vec![],
            PKind::Overlay => vec![Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<LineVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &overlay_attrs,
            })],
            PKind::Points => vec![Some(vertex_layout(wgpu::VertexStepMode::Instance))],
            _ => vec![Some(vertex_layout(wgpu::VertexStepMode::Vertex))],
        };
        let targets = [Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend, write_mask })];
        self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{kind:?}")),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some(vs),
                compilation_options: Default::default(),
                buffers: &buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: cull.then_some(wgpu::Face::Back),
                unclipped_depth: false,
                polygon_mode: polygon,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(depth_write),
                depth_compare: Some(compare),
                stencil: wgpu::StencilState::default(),
                bias: if bias {
                    wgpu::DepthBiasState { constant: 2, slope_scale: 1.5, clamp: 0.0 }
                } else {
                    wgpu::DepthBiasState::default()
                },
            }),
            multisample: wgpu::MultisampleState { count: self.sample_count, mask: !0, alpha_to_coverage_enabled: false },
            fragment: fs.map(|fs| wgpu::FragmentState {
                module: &self.module,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn ensure_targets(&mut self, egui_renderer: &mut egui_wgpu::Renderer, size: [u32; 2]) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
        let make = |label: &str, format, samples, usage, view_formats: &[wgpu::TextureFormat]| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats,
            })
        };
        let msaa = (self.sample_count > 1).then(|| {
            make("msaa color", COLOR_FORMAT, self.sample_count, wgpu::TextureUsages::RENDER_ATTACHMENT, &[])
                .create_view(&Default::default())
        });
        let depth =
            make("depth", DEPTH_FORMAT, self.sample_count, wgpu::TextureUsages::RENDER_ATTACHMENT, &[]).create_view(&Default::default());
        let resolve_tex = make(
            "resolve",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            &[EGUI_VIEW_FORMAT],
        );
        let resolve = resolve_tex.create_view(&Default::default());
        // egui expects gamma-encoded texels in an Rgba8Unorm texture: reinterpret the sRGB bytes.
        let egui_view = resolve_tex.create_view(&wgpu::TextureViewDescriptor { format: Some(EGUI_VIEW_FORMAT), ..Default::default() });
        match self.egui_id {
            Some(id) => egui_renderer.update_egui_texture_from_wgpu_texture(&self.device, &egui_view, wgpu::FilterMode::Nearest, id),
            None => self.egui_id = Some(egui_renderer.register_native_texture(&self.device, &egui_view, wgpu::FilterMode::Nearest)),
        }
        self.targets = Some(Targets { size, msaa, depth, resolve, _egui_view: egui_view });
    }

    /// Render the scene and return the egui texture showing it.
    pub fn render(&mut self, egui_renderer: &mut egui_wgpu::Renderer, size: [u32; 2], p: &FrameParams) -> egui::TextureId {
        let size = [size[0].clamp(1, 8192), size[1].clamp(1, 8192)];
        self.ensure_targets(egui_renderer, size);

        let to_lin = |c: [u8; 3]| {
            let f = |x: u8| {
                let c = x as f32 / 255.0;
                if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
            };
            [f(c[0]), f(c[1]), f(c[2]), 1.0]
        };
        let wireframe_mode = p.mode == DisplayMode::Wireframe;
        let wire_color = if wireframe_mode { [0.75, 0.82, 0.95, 1.0] } else { [0.02, 0.02, 0.03, 0.55] };
        let cam = if p.ortho { [p.back_dir.x, p.back_dir.y, p.back_dir.z, 1.0] } else { [p.eye.x, p.eye.y, p.eye.z, 0.0] };
        let u = FrameUniform {
            view_proj: p.view_proj.to_cols_array_2d(),
            cam_pos: cam,
            light_dir: [p.light_dir.x, p.light_dir.y, p.light_dir.z, p.light_intensity],
            up: [p.up.x, p.up.y, p.up.z, 0.0],
            wire_color,
            viewport: [size[0] as f32, size[1] as f32, p.point_size, 0.0],
            bg_top: to_lin(p.bg_top),
            bg_bottom: to_lin(p.bg_bottom),
            mode: p.mode.shader_id(),
            flags: u32::from(p.textures) | (u32::from(p.flat) << 1) | (u32::from(p.vertex_colors) << 2),
            _pad: [0; 2],
        };
        self.queue.write_buffer(&self.frame_buf, 0, bytemuck::bytes_of(&u));

        // Overlay lines.
        let overlay_bytes: &[u8] = bytemuck::cast_slice(&p.overlay);
        if !overlay_bytes.is_empty() {
            let need = overlay_bytes.len() as u64;
            if self.overlay_buf.as_ref().is_none_or(|(_, cap)| *cap < need) {
                let cap = need.next_power_of_two().max(4096);
                let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay"),
                    size: cap,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.overlay_buf = Some((buf, cap));
            }
            self.queue.write_buffer(&self.overlay_buf.as_ref().unwrap().0, 0, overlay_bytes);
        }

        // Resolve pipelines before borrowing targets.
        let bg_pipe = self.pipeline(PKind::Background);
        let overlay_pipe = self.pipeline(PKind::Overlay);
        let has_scene = self.scene.is_some();
        let mut draws: Vec<(wgpu::RenderPipeline, usize)> = Vec::new();
        if has_scene {
            let wire_ok = self.wire_supported;
            let mut plan: Vec<(PKind, usize)> = Vec::new();
            let scene = self.scene.as_ref().unwrap();
            let points_mode = p.mode == DisplayMode::Points;
            // Opaque first, then wire, then blended.
            for (i, m) in scene.meshes.iter().enumerate() {
                if !m.visible {
                    continue;
                }
                let cull = p.cull && !m.double_sided;
                match (m.topology, points_mode, wireframe_mode) {
                    (Topology::Points, _, _) | (Topology::Triangles, true, _) => plan.push((PKind::Points, i)),
                    (Topology::Lines, _, _) => plan.push((PKind::LineMesh, i)),
                    (Topology::Triangles, false, true) => {
                        if wire_ok {
                            plan.push((PKind::DepthOnly { cull }, i));
                        } else {
                            plan.push((PKind::Fill { cull, blend: false }, i));
                        }
                    }
                    (Topology::Triangles, false, false) => {
                        if !m.blend {
                            plan.push((PKind::Fill { cull, blend: false }, i));
                        }
                    }
                }
            }
            if wire_ok && (wireframe_mode || (p.wire_overlay && !points_mode)) {
                for (i, m) in scene.meshes.iter().enumerate() {
                    if m.visible && m.topology == Topology::Triangles {
                        plan.push((PKind::Wire, i));
                    }
                }
            }
            if !points_mode && !wireframe_mode {
                for (i, m) in scene.meshes.iter().enumerate() {
                    if m.visible && m.topology == Topology::Triangles && m.blend {
                        plan.push((PKind::Fill { cull: false, blend: true }, i));
                    }
                }
            }
            for (k, i) in plan {
                let pipe = self.pipeline(k);
                draws.push((pipe, i));
            }
        }

        let targets = self.targets.as_ref().unwrap();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scene") });
        {
            let (view, resolve, store) = match &targets.msaa {
                Some(m) => (m, Some(&targets.resolve), wgpu::StoreOp::Discard),
                None => (&targets.resolve, None, wgpu::StoreOp::Store),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.frame_bg, &[]);
            pass.set_pipeline(&bg_pipe);
            pass.draw(0..3, 0..1);

            if let Some(scene) = &self.scene {
                for (pipe, i) in &draws {
                    let m = &scene.meshes[*i];
                    pass.set_pipeline(pipe);
                    pass.set_bind_group(1, &scene.materials[m.material], &[]);
                    pass.set_bind_group(2, &scene.mesh_bg, &[m.dyn_offset]);
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    let is_points = pipe_is_points(m, p.mode);
                    if is_points {
                        pass.draw(0..6, 0..m.vertex_count);
                    } else if let Some(ib) = &m.ibuf {
                        pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..m.index_count, 0, 0..1);
                    } else {
                        pass.draw(0..m.vertex_count, 0..1);
                    }
                }
            }

            if let Some((buf, _)) = &self.overlay_buf {
                if !p.overlay.is_empty() {
                    pass.set_pipeline(&overlay_pipe);
                    pass.set_vertex_buffer(0, buf.slice(..overlay_bytes.len() as u64));
                    pass.draw(0..p.overlay.len() as u32, 0..1);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        self.egui_id.unwrap()
    }
}

fn pipe_is_points(m: &gpu_scene::GpuMesh, mode: DisplayMode) -> bool {
    m.topology == Topology::Points || (mode == DisplayMode::Points && m.topology == Topology::Triangles)
}
