//! Upload of a CPU [`Scene`] into GPU buffers, textures and bind groups.

use super::mipmap::MipGen;
use crate::scene::{AlphaMode, Material, Scene, TextureData, Topology};
use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

pub const VERTEX_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Unorm8x4];

pub fn vertex_layout(step: wgpu::VertexStepMode) -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: step, attributes: &VERTEX_ATTRS }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MaterialUniform {
    pub base_color: [f32; 4],
    pub emissive: [f32; 4],
    pub params: [f32; 4],
    pub tex: [u32; 4],
}

/// Per-mesh uniform block (dynamic offset). Layout matches `MeshU` in shader.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct MeshUniform {
    pub model: [[f32; 4]; 4],
    pub normal: [[f32; 4]; 4],
    pub flags: [u32; 4],
}

pub const MESH_UNIFORM_SIZE: u64 = std::mem::size_of::<MeshUniform>() as u64;
const FLAGS_OFFSET: u64 = 128;

pub struct GpuMesh {
    pub vbuf: wgpu::Buffer,
    pub ibuf: Option<wgpu::Buffer>,
    pub index_count: u32,
    pub vertex_count: u32,
    pub topology: Topology,
    pub material: usize,
    pub blend: bool,
    pub double_sided: bool,
    pub dyn_offset: u32,
    pub flags: [u32; 4],
    pub visible: bool,
    /// Vertex buffer can be rewritten every frame.
    pub deformable: bool,
}

pub struct GpuScene {
    pub meshes: Vec<GpuMesh>,
    pub materials: Vec<wgpu::BindGroup>,
    pub mesh_ubo: wgpu::Buffer,
    pub mesh_bg: wgpu::BindGroup,
    _textures: Vec<wgpu::Texture>,
    pub gpu_bytes: u64,
}

impl GpuScene {
    pub fn set_highlight(&mut self, queue: &wgpu::Queue, index: Option<usize>) {
        for (i, m) in self.meshes.iter_mut().enumerate() {
            let h = u32::from(Some(i) == index);
            if m.flags[2] != h {
                m.flags[2] = h;
                queue.write_buffer(&self.mesh_ubo, m.dyn_offset as u64 + FLAGS_OFFSET, bytemuck::bytes_of(&m.flags));
            }
        }
    }

    /// Set the model matrix of one mesh (animated scenes).
    pub fn set_model(&mut self, queue: &wgpu::Queue, index: usize, model: glam::Mat4) {
        let Some(m) = self.meshes.get(index) else { return };
        let u = MeshUniform { model: model.to_cols_array_2d(), normal: normal_matrix(model).to_cols_array_2d(), flags: m.flags };
        queue.write_buffer(&self.mesh_ubo, m.dyn_offset as u64, bytemuck::bytes_of(&u));
    }

    /// Replace the vertices of a deformable mesh (skinning / morphs).
    pub fn update_vertices(&mut self, queue: &wgpu::Queue, index: usize, verts: &[Vertex]) {
        let Some(m) = self.meshes.get(index) else { return };
        if m.deformable && verts.len() as u32 == m.vertex_count {
            queue.write_buffer(&m.vbuf, 0, bytemuck::cast_slice(verts));
        }
    }
}

pub fn normal_matrix(m: glam::Mat4) -> glam::Mat4 {
    let n = glam::Mat3::from_mat4(m);
    let inv = if n.determinant().abs() > 1e-12 { n.inverse().transpose() } else { n };
    glam::Mat4::from_mat3(inv)
}

/// 1x1 fallback textures bound when a material has no texture in a slot.
pub struct DefaultTextures {
    pub white_srgb: wgpu::TextureView,
    pub white_linear: wgpu::TextureView,
    pub flat_normal: wgpu::TextureView,
}

impl DefaultTextures {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let make = |rgba: [u8; 4], format: wgpu::TextureFormat| {
            let t = device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("default tex"),
                    size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &rgba,
            );
            t.create_view(&Default::default())
        };
        Self {
            white_srgb: make([255; 4], wgpu::TextureFormat::Rgba8UnormSrgb),
            white_linear: make([255; 4], wgpu::TextureFormat::Rgba8Unorm),
            flat_normal: make([128, 128, 255, 255], wgpu::TextureFormat::Rgba8Unorm),
        }
    }
}

pub struct UploadCtx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub material_bgl: &'a wgpu::BindGroupLayout,
    pub mesh_bgl: &'a wgpu::BindGroupLayout,
    pub sampler: &'a wgpu::Sampler,
    pub defaults: &'a DefaultTextures,
    pub mipgen: &'a mut MipGen,
}

fn upload_texture(
    ctx: &mut UploadCtx,
    encoder: &mut wgpu::CommandEncoder,
    data: &TextureData,
    srgb: bool,
) -> (wgpu::Texture, wgpu::TextureView, u64) {
    let max = ctx.device.limits().max_texture_dimension_2d;
    let resized;
    let (w, h, bytes): (u32, u32, &[u8]) = if data.width > max || data.height > max {
        let s = max as f32 / data.width.max(data.height) as f32;
        let (nw, nh) = (((data.width as f32 * s) as u32).max(1), ((data.height as f32 * s) as u32).max(1));
        let img = image::RgbaImage::from_raw(data.width, data.height, data.rgba.clone()).expect("rgba size");
        resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle).into_raw();
        (nw, nh, &resized)
    } else {
        (data.width.max(1), data.height.max(1), &data.rgba)
    };
    let mips = 32 - w.max(h).leading_zeros();
    let format = if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&data.name),
        size,
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    if bytes.len() as u64 >= w as u64 * h as u64 * 4 {
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
            size,
        );
    }
    ctx.mipgen.generate(ctx.device, encoder, &tex, format, mips);
    let view = tex.create_view(&Default::default());
    (tex, view, w as u64 * h as u64 * 4 * 4 / 3)
}

pub fn upload(ctx: &mut UploadCtx, scene: &Scene) -> GpuScene {
    let device = ctx.device;
    let mut gpu_bytes = 0u64;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("upload") });

    // Textures, created per (index, sRGB) usage.
    let mut wanted: Vec<(usize, bool)> = Vec::new();
    for m in &scene.materials {
        for (t, srgb) in
            [(m.base_color_tex, true), (m.emissive_tex, true), (m.normal_tex, false), (m.mr_tex, false), (m.occlusion_tex, false)]
        {
            if let Some(t) = t.filter(|&t| t < scene.textures.len()) {
                if !wanted.contains(&(t, srgb)) {
                    wanted.push((t, srgb));
                }
            }
        }
    }
    let mut textures = Vec::new();
    let mut views: HashMap<(usize, bool), wgpu::TextureView> = HashMap::new();
    for (t, srgb) in wanted {
        let (tex, view, bytes) = upload_texture(ctx, &mut encoder, &scene.textures[t], srgb);
        gpu_bytes += bytes;
        textures.push(tex);
        views.insert((t, srgb), view);
    }

    // Materials (+ one default at the end).
    let default_mat = Material::default();
    let mut materials = Vec::new();
    for m in scene.materials.iter().chain(std::iter::once(&default_mat)) {
        let get = |t: Option<usize>, srgb: bool| t.and_then(|t| views.get(&(t, srgb)));
        let slots = [
            (get(m.base_color_tex, true), &ctx.defaults.white_srgb),
            (get(m.normal_tex, false), &ctx.defaults.flat_normal),
            (get(m.mr_tex, false), &ctx.defaults.white_linear),
            (get(m.occlusion_tex, false), &ctx.defaults.white_linear),
            (get(m.emissive_tex, true), &ctx.defaults.white_srgb),
        ];
        let mut mask = 0u32;
        for (i, (v, _)) in slots.iter().enumerate() {
            if v.is_some() {
                mask |= 1 << i;
            }
        }
        let u = MaterialUniform {
            base_color: m.base_color,
            emissive: [m.emissive[0], m.emissive[1], m.emissive[2], 0.0],
            params: [
                m.metallic,
                m.roughness,
                m.alpha_cutoff,
                match m.alpha_mode {
                    AlphaMode::Opaque => 0.0,
                    AlphaMode::Mask => 1.0,
                    AlphaMode::Blend => 2.0,
                },
            ],
            tex: [mask, 0, 0, 0],
        };
        let ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("material ubo"),
            contents: bytemuck::bytes_of(&u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: ubo.as_entire_binding() }];
        for (i, (v, d)) in slots.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry { binding: 1 + i as u32, resource: wgpu::BindingResource::TextureView(v.unwrap_or(d)) });
        }
        entries.push(wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(ctx.sampler) });
        materials.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material bg"),
            layout: ctx.material_bgl,
            entries: &entries,
        }));
    }
    let default_index = materials.len() - 1;

    // Per-mesh uniforms with dynamic offsets.
    let stride = device.limits().min_uniform_buffer_offset_alignment.max(MESH_UNIFORM_SIZE as u32).next_multiple_of(256);
    let count = scene.meshes.len().max(1);
    let mut mesh_data = vec![0u8; stride as usize * count];
    let mut meshes = Vec::new();
    for (i, m) in scene.meshes.iter().enumerate() {
        let has_uv = !m.uvs.is_empty();
        let has_c = !m.colors.is_empty();
        let verts: Vec<Vertex> = (0..m.positions.len())
            .map(|k| Vertex {
                pos: m.positions[k],
                normal: m.normals.get(k).copied().unwrap_or([0.0; 3]),
                uv: if has_uv { m.uvs[k] } else { [0.0; 2] },
                color: if has_c { m.colors[k] } else { [255; 4] },
            })
            .collect();
        let deformable = scene.anim.is_some() && crate::anim::needs_deform(m);
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&m.name),
            contents: bytemuck::cast_slice(&verts),
            usage: if deformable { wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST } else { wgpu::BufferUsages::VERTEX },
        });
        gpu_bytes += (verts.len() * std::mem::size_of::<Vertex>()) as u64;
        let ibuf = (!m.indices.is_empty() && m.topology != Topology::Points).then(|| {
            gpu_bytes += m.indices.len() as u64 * 4;
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("indices"),
                contents: bytemuck::cast_slice(&m.indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        });
        let mat = m.material.filter(|&i| i < scene.materials.len());
        let mref = mat.map(|i| &scene.materials[i]);
        let flags = [u32::from(has_uv), u32::from(has_c), 0, u32::from(m.normals.iter().any(|n| n != &[0.0; 3]))];
        let off = i as u32 * stride;
        let mu = MeshUniform { model: glam::Mat4::IDENTITY.to_cols_array_2d(), normal: glam::Mat4::IDENTITY.to_cols_array_2d(), flags };
        mesh_data[off as usize..off as usize + MESH_UNIFORM_SIZE as usize].copy_from_slice(bytemuck::bytes_of(&mu));
        meshes.push(GpuMesh {
            vbuf,
            ibuf,
            index_count: if m.topology == Topology::Points { 0 } else { m.indices.len() as u32 },
            vertex_count: m.positions.len() as u32,
            topology: m.topology,
            material: mat.unwrap_or(default_index),
            blend: mref.is_some_and(|x| x.alpha_mode == AlphaMode::Blend),
            double_sided: mref.is_none_or(|x| x.double_sided),
            dyn_offset: off,
            flags,
            visible: true,
            deformable,
        });
    }
    let mesh_ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mesh ubo"),
        contents: &mesh_data,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let mesh_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh bg"),
        layout: ctx.mesh_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &mesh_ubo,
                offset: 0,
                size: wgpu::BufferSize::new(MESH_UNIFORM_SIZE),
            }),
        }],
    });
    ctx.queue.submit([encoder.finish()]);
    GpuScene { meshes, materials, mesh_ubo, mesh_bg, _textures: textures, gpu_bytes }
}
