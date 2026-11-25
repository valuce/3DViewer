//! Autodesk 3D Studio (.3ds) loader.
//!
//! A 3DS file is a tree of chunks: u16 id + u32 length (including the 6-byte header), little endian.
//! Handled: triangle meshes (vertices, faces, UVs, per-face material groups, smoothing groups) and
//! standard materials (diffuse colour, transparency, self-illumination, two-sided, diffuse map).
//! Vertices are stored in world space, so the per-object matrix (0x4160) is only used to detect
//! mirrored objects, whose face winding must be flipped. The keyframer (0xB000) is ignored.

use super::image::{ImageSource, decode_parallel, resolve_texture_path};
use super::{read_f32_le, read_u32_le};
use crate::scene::{AlphaMode, Material, Mesh, Scene, Topology};
use anyhow::{Result, bail};
use glam::{Mat3, Vec3};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAIN: u16 = 0x4D4D;
const VERSION: u16 = 0x0002;
const EDITOR: u16 = 0x3D3D;
const MASTER_SCALE: u16 = 0x0100;
const OBJECT: u16 = 0x4000;
const TRI_MESH: u16 = 0x4100;
const VERTICES: u16 = 0x4110;
const FACES: u16 = 0x4120;
const FACE_MATERIAL: u16 = 0x4130;
const UVS: u16 = 0x4140;
const SMOOTHING: u16 = 0x4150;
const MESH_MATRIX: u16 = 0x4160;
const LIGHT: u16 = 0x4600;
const CAMERA: u16 = 0x4700;
const MATERIAL: u16 = 0xAFFF;
const MAT_NAME: u16 = 0xA000;
const MAT_AMBIENT: u16 = 0xA010;
const MAT_DIFFUSE: u16 = 0xA020;
const MAT_TRANSPARENCY: u16 = 0xA050;
const MAT_TWO_SIDED: u16 = 0xA081;
const MAT_SELF_ILLUM: u16 = 0xA084;
const MAT_TEXTURE: u16 = 0xA200;
const MAP_FILENAME: u16 = 0xA300;
const COLOR_F: u16 = 0x0010;
const COLOR_24: u16 = 0x0011;
const LIN_COLOR_24: u16 = 0x0012;
const LIN_COLOR_F: u16 = 0x0013;
const PERCENT_U16: u16 = 0x0030;
const PERCENT_F32: u16 = 0x0031;

pub fn parse_3ds(data: &[u8], dir: Option<&Path>) -> Result<Scene> {
    if data.len() < 6 || u16::from_le_bytes([data[0], data[1]]) != MAIN {
        bail!("not a 3DS file (missing main chunk 0x4D4D)");
    }
    let mut p = Parser::default();
    let mut top = Chunks::new(data);
    match top.next() {
        Some((_, body)) => p.parse_main(body),
        None => bail!("3DS main chunk has an invalid length"),
    }
    // Bytes after the main chunk are ignored; only a cut-off main chunk counts as damage.
    p.truncated |= top.bad;

    let mut scene = Scene::default();
    scene.info.format = "3DS".into();
    scene.info.up_axis_z = true;
    scene.info.nodes = p.nodes;
    scene.info.lights = p.lights;
    scene.info.cameras = p.cameras;
    if let Some(v) = p.version {
        scene.info.extra.push(("3DS version".into(), v.to_string()));
    }
    if let Some(s) = p.master_scale.filter(|s| *s != 1.0) {
        scene.info.extra.push(("master scale".into(), s.to_string()));
    }

    load_materials(&mut scene, &p.materials, dir);
    let mut mat_index: HashMap<&str, usize> = HashMap::new();
    for (i, m) in p.materials.iter().enumerate() {
        mat_index.entry(m.name.as_str()).or_insert(i);
    }
    let mut unknown = Vec::new();
    for raw in &p.meshes {
        build_meshes(raw, &mat_index, &scene.materials, &mut scene.meshes, &mut scene.info.warnings, &mut unknown);
    }
    unknown.sort();
    unknown.dedup();
    for name in unknown {
        scene.info.warnings.push(format!("unknown material: {name}"));
    }
    if p.truncated {
        scene.info.warnings.push("file is truncated or has invalid chunk lengths; some data may be missing".into());
    }
    if scene.meshes.is_empty() {
        if p.truncated {
            bail!("3DS file is truncated and contains no triangle meshes");
        }
        bail!("3DS file contains no triangle meshes");
    }
    Ok(scene)
}

/// Iterates the chunks packed in `data`. A length that overflows the parent is truncated to the
/// bytes available (and `bad` is set); a length below the header size stops the iteration.
struct Chunks<'a> {
    data: &'a [u8],
    pos: usize,
    bad: bool,
}

impl<'a> Chunks<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, bad: false }
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let rest = &self.data[self.pos..];
        if rest.is_empty() {
            return None;
        }
        if rest.len() < 6 {
            // A few zero bytes of padding are harmless; anything else is a cut-off header.
            self.bad |= rest.iter().any(|&b| b != 0);
            self.pos = self.data.len();
            return None;
        }
        let id = u16::from_le_bytes([rest[0], rest[1]]);
        let len = read_u32_le(rest, 2) as usize;
        if len < 6 {
            self.bad = true;
            self.pos = self.data.len();
            return None;
        }
        let end = if len > rest.len() {
            self.bad = true;
            rest.len()
        } else {
            len
        };
        self.pos += end;
        Some((id, &rest[6..end]))
    }
}

#[derive(Default)]
struct RawMaterial {
    name: String,
    /// sRGB 0..1.
    diffuse: Option<[f32; 3]>,
    ambient: Option<[f32; 3]>,
    transparency: f32,
    self_illum: f32,
    two_sided: bool,
    texture: Option<String>,
}

#[derive(Default)]
struct RawMesh {
    name: String,
    positions: Vec<[f32; 3]>,
    /// Already flipped to the glTF convention (V down).
    uvs: Vec<[f32; 2]>,
    faces: Vec<[u16; 3]>,
    /// Material name and the faces it covers.
    groups: Vec<(String, Vec<u16>)>,
    smoothing: Vec<u32>,
    mirrored: bool,
}

#[derive(Default)]
struct Parser {
    materials: Vec<RawMaterial>,
    meshes: Vec<RawMesh>,
    nodes: usize,
    lights: usize,
    cameras: usize,
    version: Option<u32>,
    master_scale: Option<f32>,
    /// A chunk was cut off or claimed more bytes than its parent holds.
    truncated: bool,
}

impl Parser {
    fn check(&mut self, it: &Chunks) {
        self.truncated |= it.bad;
    }

    fn parse_main(&mut self, body: &[u8]) {
        let mut it = Chunks::new(body);
        for (id, b) in it.by_ref() {
            match id {
                VERSION if b.len() >= 4 => self.version = Some(read_u32_le(b, 0)),
                EDITOR => self.parse_editor(b),
                _ => {}
            }
        }
        self.check(&it);
    }

    fn parse_editor(&mut self, body: &[u8]) {
        let mut it = Chunks::new(body);
        for (id, b) in it.by_ref() {
            match id {
                MATERIAL => self.parse_material(b),
                OBJECT => self.parse_object(b),
                MASTER_SCALE if b.len() >= 4 => self.master_scale = Some(read_f32_le(b, 0)),
                _ => {}
            }
        }
        self.check(&it);
    }

    fn parse_object(&mut self, body: &[u8]) {
        self.nodes += 1;
        let (name, rest) = self.cstr(body);
        let mut it = Chunks::new(rest);
        for (id, b) in it.by_ref() {
            match id {
                TRI_MESH => self.parse_tri_mesh(&name, b),
                LIGHT => self.lights += 1,
                CAMERA => self.cameras += 1,
                _ => {}
            }
        }
        self.check(&it);
    }

    fn parse_tri_mesh(&mut self, name: &str, body: &[u8]) {
        let mut m = RawMesh { name: name.to_string(), ..Default::default() };
        let mut it = Chunks::new(body);
        for (id, b) in it.by_ref() {
            match id {
                VERTICES => {
                    let n = self.counted(b, 12);
                    m.positions = (0..n)
                        .map(|i| {
                            let o = 2 + i * 12;
                            [read_f32_le(b, o), read_f32_le(b, o + 4), read_f32_le(b, o + 8)]
                        })
                        .collect();
                }
                UVS => {
                    let n = self.counted(b, 8);
                    m.uvs = (0..n).map(|i| [read_f32_le(b, 2 + i * 8), 1.0 - read_f32_le(b, 6 + i * 8)]).collect();
                }
                FACES => self.parse_faces(&mut m, b),
                MESH_MATRIX if b.len() >= 36 => {
                    let mut axes = [0f32; 9];
                    for (i, a) in axes.iter_mut().enumerate() {
                        *a = read_f32_le(b, i * 4);
                    }
                    m.mirrored = Mat3::from_cols_array(&axes).determinant() < 0.0;
                }
                _ => {}
            }
        }
        self.check(&it);
        self.meshes.push(m);
    }

    /// Face list: u16 count, count × (a, b, c, flags), then sub-chunks (material groups, smoothing).
    fn parse_faces(&mut self, m: &mut RawMesh, b: &[u8]) {
        let declared = if b.len() >= 2 { read_u16(b, 0) as usize } else { 0 };
        let n = self.counted(b, 8);
        m.faces = (0..n).map(|i| [read_u16(b, 2 + i * 8), read_u16(b, 4 + i * 8), read_u16(b, 6 + i * 8)]).collect();
        if n < declared || b.len() < 2 {
            return; // cut off: whatever follows is not a sub-chunk
        }
        let mut it = Chunks::new(&b[2 + n * 8..]);
        for (id, s) in it.by_ref() {
            match id {
                FACE_MATERIAL => {
                    let (name, rest) = self.cstr(s);
                    let k = self.counted(rest, 2);
                    m.groups.push((name, (0..k).map(|i| read_u16(rest, 2 + i * 2)).collect()));
                }
                SMOOTHING => {
                    let k = s.len() / 4;
                    self.truncated |= k < n;
                    m.smoothing = (0..k.min(n)).map(|i| read_u32_le(s, i * 4)).collect();
                }
                _ => {}
            }
        }
        self.check(&it);
    }

    fn parse_material(&mut self, body: &[u8]) {
        let mut mat = RawMaterial::default();
        let mut it = Chunks::new(body);
        for (id, b) in it.by_ref() {
            match id {
                MAT_NAME => mat.name = self.cstr(b).0,
                MAT_DIFFUSE => mat.diffuse = self.color(b),
                MAT_AMBIENT => mat.ambient = self.color(b),
                MAT_TRANSPARENCY => mat.transparency = self.percent(b).unwrap_or(0.0),
                MAT_SELF_ILLUM => mat.self_illum = self.percent(b).unwrap_or(0.0),
                MAT_TWO_SIDED => mat.two_sided = true,
                MAT_TEXTURE => mat.texture = self.map_name(b),
                // Specular colour, shininess, bump (0xA230: a height map, not a normal map),
                // opacity and other maps are ignored.
                _ => {}
            }
        }
        self.check(&it);
        self.materials.push(mat);
    }

    /// Colour chunk: prefers the "linear" variant (0x0012/0x0013), else the first plain one.
    /// The values are display colours (sRGB) either way.
    fn color(&mut self, b: &[u8]) -> Option<[f32; 3]> {
        let (mut plain, mut lin) = (None, None);
        let mut it = Chunks::new(b);
        for (id, c) in it.by_ref() {
            let v = match id {
                COLOR_F | LIN_COLOR_F if c.len() >= 12 => [read_f32_le(c, 0), read_f32_le(c, 4), read_f32_le(c, 8)],
                COLOR_24 | LIN_COLOR_24 if c.len() >= 3 => [c[0], c[1], c[2]].map(|x| x as f32 / 255.0),
                _ => continue,
            };
            let slot = if matches!(id, LIN_COLOR_24 | LIN_COLOR_F) { &mut lin } else { &mut plain };
            slot.get_or_insert(v);
        }
        self.check(&it);
        lin.or(plain).map(|c| c.map(|x| if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 }))
    }

    /// Percentage chunk as 0..1: 0x0030 is u16 0..100, 0x0031 is f32 (0..1, or 0..100 in some files).
    fn percent(&mut self, b: &[u8]) -> Option<f32> {
        let mut out = None;
        let mut it = Chunks::new(b);
        for (id, c) in it.by_ref() {
            let v = match id {
                PERCENT_U16 if c.len() >= 2 => read_u16(c, 0) as f32 / 100.0,
                PERCENT_F32 if c.len() >= 4 => {
                    let f = read_f32_le(c, 0);
                    if f > 1.0 { f / 100.0 } else { f }
                }
                _ => continue,
            };
            out.get_or_insert(v);
        }
        self.check(&it);
        out.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0))
    }

    fn map_name(&mut self, b: &[u8]) -> Option<String> {
        let mut name = None;
        let mut it = Chunks::new(b);
        for (id, c) in it.by_ref() {
            if id == MAP_FILENAME {
                let n = self.cstr(c).0.trim().to_string();
                if !n.is_empty() {
                    name = Some(n);
                }
            }
        }
        self.check(&it);
        name
    }

    /// Element count (u16) of a record array, clamped to the records actually present.
    fn counted(&mut self, b: &[u8], record: usize) -> usize {
        if b.len() < 2 {
            self.truncated = true;
            return 0;
        }
        let n = read_u16(b, 0) as usize;
        let avail = (b.len() - 2) / record;
        if n > avail {
            self.truncated = true;
            avail
        } else {
            n
        }
    }

    /// Null-terminated string. Names use the author's ANSI code page: bytes that are not valid
    /// UTF-8 are kept one-to-one as Latin-1 instead of being replaced.
    fn cstr<'a>(&mut self, b: &'a [u8]) -> (String, &'a [u8]) {
        let (s, rest): (&[u8], &[u8]) = match b.iter().position(|&c| c == 0) {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => {
                self.truncated = true;
                (b, &[])
            }
        };
        let s = match std::str::from_utf8(s) {
            Ok(s) => s.to_string(),
            Err(_) => s.iter().map(|&c| c as char).collect(),
        };
        (s, rest)
    }
}

fn read_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn load_materials(scene: &mut Scene, raw: &[RawMaterial], dir: Option<&Path>) {
    let mut jobs: Vec<(String, ImageSource)> = Vec::new();
    let mut slot_of: HashMap<String, Option<usize>> = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut listing: Option<Vec<(String, PathBuf)>> = None;
    let mut wanted: Vec<Option<usize>> = Vec::with_capacity(raw.len());
    for rm in raw {
        let mut m = Material { name: rm.name.clone(), ..Default::default() };
        if let Some(c) = rm.diffuse.or(rm.ambient) {
            let [r, g, b] = c.map(srgb_to_linear);
            m.base_color = [r, g, b, 1.0];
        }
        let alpha = (1.0 - rm.transparency).clamp(0.0, 1.0);
        m.base_color[3] = alpha;
        if alpha < 0.999 {
            m.alpha_mode = AlphaMode::Blend;
        }
        m.double_sided = rm.two_sided;
        m.emissive = [m.base_color[0] * rm.self_illum, m.base_color[1] * rm.self_illum, m.base_color[2] * rm.self_illum];
        let slot = rm.texture.as_deref().and_then(|name| {
            *slot_of.entry(name.to_lowercase()).or_insert_with(|| match find_texture(dir, name, &mut listing) {
                Some(path) => {
                    jobs.push((file_name(name).to_string(), ImageSource::File(path)));
                    Some(jobs.len() - 1)
                }
                None => {
                    missing.push(name.to_string());
                    None
                }
            })
        });
        wanted.push(slot);
        scene.materials.push(m);
    }

    let names: Vec<String> = jobs.iter().map(|j| j.0.clone()).collect();
    let mut job_to_tex = Vec::with_capacity(jobs.len());
    for (i, r) in decode_parallel(jobs).into_iter().enumerate() {
        match r {
            Ok(t) => {
                job_to_tex.push(Some(scene.textures.len()));
                scene.textures.push(t);
            }
            Err(e) => {
                scene.info.warnings.push(format!("texture {}: {e}", names[i]));
                job_to_tex.push(None);
            }
        }
    }
    for ((m, rm), slot) in scene.materials.iter_mut().zip(raw).zip(wanted) {
        let Some(t) = slot.and_then(|j| job_to_tex[j]) else { continue };
        // With the usual 100 % map amount 3ds Max shows the map instead of the diffuse colour.
        m.base_color_tex = Some(t);
        m.base_color[..3].copy_from_slice(&[1.0; 3]);
        if rm.self_illum > 0.0 {
            m.emissive = [rm.self_illum; 3];
            m.emissive_tex = Some(t);
        }
    }
    missing.sort();
    missing.dedup();
    for f in missing {
        scene.info.warnings.push(format!("texture not found: {f}"));
    }
}

/// 3DS stores DOS 8.3 names, often upper case. After the usual lookup, match the file name
/// case-insensitively in the model folder and its texture-like subfolders.
fn find_texture(dir: Option<&Path>, name: &str, listing: &mut Option<Vec<(String, PathBuf)>>) -> Option<PathBuf> {
    if let Some(p) = resolve_texture_path(dir, &[name]) {
        return Some(p);
    }
    let dir = dir?;
    let wanted = file_name(name).to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    listing.get_or_insert_with(|| list_texture_files(dir)).iter().find(|(n, _)| *n == wanted).map(|(_, p)| p.clone())
}

fn file_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// (lower-case file name, path) for files in `dir` and its texture subfolders, `dir` first.
fn list_texture_files(dir: &Path) -> Vec<(String, PathBuf)> {
    const SUBDIRS: &[&str] = &["textures", "texture", "tex", "maps", "images"];
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    let mut i = 0;
    while i < dirs.len() {
        if let Ok(rd) = std::fs::read_dir(&dirs[i]) {
            for e in rd.flatten() {
                let lower = e.file_name().to_string_lossy().to_lowercase();
                match e.file_type() {
                    Ok(t) if t.is_dir() => {
                        if i == 0 && SUBDIRS.contains(&lower.as_str()) {
                            dirs.push(e.path());
                        }
                    }
                    Ok(_) => out.push((lower, e.path())),
                    Err(_) => {}
                }
            }
        }
        i += 1;
    }
    out
}

/// Turn one 3DS object into one mesh per material, with normals from its smoothing groups.
fn build_meshes(
    raw: &RawMesh,
    mat_index: &HashMap<&str, usize>,
    materials: &[Material],
    out: &mut Vec<Mesh>,
    warnings: &mut Vec<String>,
    unknown: &mut Vec<String>,
) {
    const SKIP: u32 = u32::MAX;
    const UNGROUPED: u32 = u32::MAX - 1;
    const END: u32 = u32::MAX;
    let nv = raw.positions.len();
    if nv == 0 || raw.faces.is_empty() {
        return;
    }

    // Faces with winding fixed for mirrored objects; faces with bad indices are skipped.
    let faces: Vec<[u32; 3]> = raw
        .faces
        .iter()
        .map(|f| {
            let f = f.map(u32::from);
            if raw.mirrored { [f[0], f[2], f[1]] } else { f }
        })
        .collect();
    let mut part_of: Vec<u32> = faces.iter().map(|f| if f.iter().all(|&i| (i as usize) < nv) { UNGROUPED } else { SKIP }).collect();
    let bad = part_of.iter().filter(|&&p| p == SKIP).count();
    if bad > 0 {
        warnings.push(format!("object '{}': {bad} faces reference missing vertices", raw.name));
    }

    // Output parts, one per material (None = faces without a known material).
    let mut parts: Vec<Option<usize>> = Vec::new();
    let part_for = |mat: Option<usize>, parts: &mut Vec<Option<usize>>| -> u32 {
        let i = parts.iter().position(|&p| p == mat).unwrap_or_else(|| {
            parts.push(mat);
            parts.len() - 1
        });
        i as u32
    };
    for (name, list) in &raw.groups {
        let mat = mat_index.get(name.as_str()).copied();
        if mat.is_none() {
            unknown.push(name.clone());
        }
        let part = part_for(mat, &mut parts);
        for &f in list {
            if let Some(slot) = part_of.get_mut(f as usize).filter(|s| **s == UNGROUPED) {
                *slot = part;
            }
        }
    }
    if part_of.contains(&UNGROUPED) {
        let part = part_for(None, &mut parts);
        for p in part_of.iter_mut().filter(|p| **p == UNGROUPED) {
            *p = part;
        }
    }

    let uvs = &raw.uvs;
    if !uvs.is_empty() && uvs.len() != nv {
        warnings.push(format!("object '{}': {} UVs for {nv} vertices", raw.name, uvs.len()));
    }

    // Area-weighted face normals.
    let pos = |i: u32| Vec3::from(raw.positions[i as usize]);
    let face_n: Vec<Vec3> = faces
        .iter()
        .zip(&part_of)
        .map(|(f, &p)| if p == SKIP { Vec3::ZERO } else { (pos(f[1]) - pos(f[0])).cross(pos(f[2]) - pos(f[0])) })
        .collect();

    // Weld equal positions so smoothing also works across UV seams (vertices split for UVs).
    let mut weld: HashMap<[u32; 3], u32> = HashMap::with_capacity(nv);
    let pos_id: Vec<u32> = raw
        .positions
        .iter()
        .map(|p| {
            let next = weld.len() as u32;
            *weld.entry(p.map(|c| (c + 0.0).to_bits())).or_insert(next)
        })
        .collect();
    let npos = weld.len();

    // Faces around each welded position (CSR).
    let mut start = vec![0u32; npos + 1];
    for (f, &p) in faces.iter().zip(&part_of) {
        if p != SKIP {
            for &v in f {
                start[pos_id[v as usize] as usize + 1] += 1;
            }
        }
    }
    for i in 0..npos {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut around = vec![0u32; start[npos] as usize];
    for (fi, (f, &p)) in faces.iter().zip(&part_of).enumerate() {
        if p != SKIP {
            for &v in f {
                let s = &mut fill[pos_id[v as usize] as usize];
                around[*s as usize] = fi as u32;
                *s += 1;
            }
        }
    }

    // Per-position cache of smoothed normals by mask: (next, mask, normal).
    let mut cache_head = vec![END; npos];
    let mut cache: Vec<(u32, u32, Vec3)> = Vec::new();
    // Per-source-vertex list of emitted vertices: (next, part, normal bits, output index).
    let mut vert_head = vec![END; nv];
    let mut verts: Vec<(u32, u32, [u32; 3], u32)> = Vec::new();

    let mut meshes: Vec<Mesh> = parts
        .iter()
        .map(|&mat| {
            let mut m = Mesh::new(raw.name.clone(), Topology::Triangles);
            m.material = mat;
            m.had_normals = false;
            m
        })
        .collect();
    let smooth = &raw.smoothing;
    for (fi, f) in faces.iter().enumerate() {
        let part = part_of[fi];
        if part == SKIP {
            continue;
        }
        let mask = smooth.get(fi).copied().unwrap_or(0);
        let flat = face_n[fi].normalize_or(Vec3::Z);
        for &v in f {
            // Corner normal: sum of the faces around this position that share a smoothing group.
            let n = if mask == 0 {
                flat
            } else {
                let pid = pos_id[v as usize] as usize;
                let mut e = cache_head[pid];
                while e != END && cache[e as usize].1 != mask {
                    e = cache[e as usize].0;
                }
                if e != END {
                    cache[e as usize].2
                } else {
                    let mut sum = Vec3::ZERO;
                    for &g in &around[start[pid] as usize..start[pid + 1] as usize] {
                        if smooth.get(g as usize).is_some_and(|&gm| gm & mask != 0) {
                            sum += face_n[g as usize];
                        }
                    }
                    let n = sum.normalize_or(flat);
                    cache.push((cache_head[pid], mask, n));
                    cache_head[pid] = (cache.len() - 1) as u32;
                    n
                }
            };
            let bits = n.to_array().map(f32::to_bits);
            let mut e = vert_head[v as usize];
            while e != END && (verts[e as usize].1 != part || verts[e as usize].2 != bits) {
                e = verts[e as usize].0;
            }
            let mesh = &mut meshes[part as usize];
            let idx = if e != END {
                verts[e as usize].3
            } else {
                let idx = mesh.positions.len() as u32;
                mesh.positions.push(raw.positions[v as usize]);
                mesh.normals.push(n.to_array());
                if !uvs.is_empty() {
                    mesh.uvs.push(uvs.get(v as usize).copied().unwrap_or([0.0, 1.0]));
                }
                verts.push((vert_head[v as usize], part, bits, idx));
                vert_head[v as usize] = (verts.len() - 1) as u32;
                idx
            };
            mesh.indices.push(idx);
        }
    }

    let multi = meshes.iter().filter(|m| !m.indices.is_empty()).count() > 1;
    for mut m in meshes.into_iter().filter(|m| !m.indices.is_empty()) {
        if let (true, Some(mat)) = (multi, m.material.and_then(|i| materials.get(i))) {
            m.name = format!("{} ({})", raw.name, mat.name);
        }
        out.push(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: u16, parts: &[&[u8]]) -> Vec<u8> {
        let body = parts.concat();
        let mut v = id.to_le_bytes().to_vec();
        v.extend_from_slice(&(body.len() as u32 + 6).to_le_bytes());
        v.extend_from_slice(&body);
        v
    }

    fn cstr(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn u16s(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn u32s(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[derive(Default)]
    struct Obj {
        name: &'static str,
        verts: Vec<[f32; 3]>,
        faces: Vec<[u16; 3]>,
        uvs: Vec<[f32; 2]>,
        smoothing: Option<Vec<u32>>,
        groups: Vec<(&'static str, Vec<u16>)>,
        matrix: Option<[f32; 12]>,
    }

    impl Obj {
        fn bytes(&self) -> Vec<u8> {
            let mut mesh = chunk(VERTICES, &[&u16s(&[self.verts.len() as u16]), &f32s(&self.verts.concat())]);
            if !self.uvs.is_empty() {
                mesh.extend(chunk(UVS, &[&u16s(&[self.uvs.len() as u16]), &f32s(&self.uvs.concat())]));
            }
            let mut f = u16s(&[self.faces.len() as u16]);
            for t in &self.faces {
                f.extend(u16s(&[t[0], t[1], t[2], 7]));
            }
            for (name, list) in &self.groups {
                f.extend(chunk(FACE_MATERIAL, &[&cstr(name), &u16s(&[list.len() as u16]), &u16s(list)]));
            }
            if let Some(s) = &self.smoothing {
                f.extend(chunk(SMOOTHING, &[&u32s(s)]));
            }
            mesh.extend(chunk(FACES, &[&f]));
            if let Some(m) = &self.matrix {
                mesh.extend(chunk(MESH_MATRIX, &[&f32s(m)]));
            }
            chunk(OBJECT, &[&cstr(self.name), &chunk(TRI_MESH, &[&mesh])])
        }
    }

    fn material(name: &str, rgb: [u8; 3], texture: Option<&str>) -> Vec<u8> {
        let mut body = chunk(MAT_NAME, &[&cstr(name)]);
        body.extend(chunk(MAT_DIFFUSE, &[&chunk(COLOR_24, &[&rgb])]));
        if let Some(t) = texture {
            body.extend(chunk(MAT_TEXTURE, &[&chunk(PERCENT_U16, &[&u16s(&[100])]), &chunk(MAP_FILENAME, &[&cstr(t)])]));
        }
        chunk(MATERIAL, &[&body])
    }

    fn file(items: &[Vec<u8>]) -> Vec<u8> {
        chunk(MAIN, &[&chunk(VERSION, &[&3u32.to_le_bytes()]), &chunk(EDITOR, &[&items.concat()])])
    }

    const QUAD: [[f32; 3]; 4] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];

    fn sample() -> Vec<u8> {
        let obj = Obj {
            name: "Box",
            verts: QUAD.to_vec(),
            faces: vec![[0, 1, 2], [0, 2, 3]],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            smoothing: Some(vec![1, 1]),
            groups: vec![("Brick", vec![0, 1])],
            ..Default::default()
        };
        file(&[material("Brick", [255, 128, 0], Some("BRICK.PNG")), obj.bytes()])
    }

    fn close(a: [f32; 3], b: Vec3) -> bool {
        (Vec3::from(a) - b).length() < 1e-5
    }

    #[test]
    fn object_with_material_uvs_and_missing_texture() {
        let s = parse_3ds(&sample(), None).unwrap();
        assert_eq!(s.info.format, "3DS");
        assert!(s.info.up_axis_z);
        assert_eq!(s.info.nodes, 1);
        assert_eq!(s.meshes.len(), 1);
        let m = &s.meshes[0];
        assert_eq!(m.name, "Box");
        assert_eq!(m.positions, QUAD.to_vec());
        assert_eq!(m.indices, vec![0, 1, 2, 0, 2, 3]);
        assert_eq!(m.uvs, vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
        assert!(m.normals.iter().all(|&n| close(n, Vec3::Z)));
        assert!(!m.had_normals);
        assert_eq!(m.material, Some(0));
        let mat = &s.materials[0];
        assert_eq!(mat.name, "Brick");
        let want = [1.0, 0.21586, 0.0, 1.0];
        assert!(mat.base_color.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-4), "{:?}", mat.base_color);
        assert_eq!(mat.alpha_mode, AlphaMode::Opaque);
        assert_eq!(mat.base_color_tex, None);
        assert!(s.info.warnings.iter().any(|w| w == "texture not found: BRICK.PNG"), "{:?}", s.info.warnings);
    }

    #[test]
    fn texture_found_case_insensitively_in_maps_folder() {
        let dir = std::env::temp_dir().join(format!("viewer3d_3ds_test_{}", std::process::id()));
        let maps = dir.join("MAPS");
        std::fs::create_dir_all(&maps).unwrap();
        ::image::RgbaImage::from_pixel(2, 2, ::image::Rgba([10, 20, 30, 255])).save(maps.join("brick.png")).unwrap();
        let s = parse_3ds(&sample(), Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        let s = s.unwrap();
        assert_eq!(s.textures.len(), 1);
        assert_eq!((s.textures[0].width, s.textures[0].height), (2, 2));
        assert_eq!(s.materials[0].base_color_tex, Some(0));
        assert_eq!(s.materials[0].base_color, [1.0; 4]);
        assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);
    }

    fn folded(verts: Vec<[f32; 3]>, faces: Vec<[u16; 3]>, smoothing: Option<Vec<u32>>) -> Mesh {
        let obj = Obj { name: "Fold", verts, faces, smoothing, ..Default::default() };
        let mut s = parse_3ds(&file(&[obj.bytes()]), None).unwrap();
        assert_eq!(s.meshes.len(), 1);
        assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);
        s.meshes.remove(0)
    }

    #[test]
    fn smoothing_groups_control_shared_normals() {
        // Two faces folded along the 0-2 edge.
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 1.0]];
        let faces = vec![[0, 1, 2], [0, 2, 3]];
        let face_n = [Vec3::Z, Vec3::new(1.0, -1.0, 1.0).normalize()];

        // Disjoint groups: shared vertices are split and every corner keeps its face normal.
        let m = folded(verts.clone(), faces.clone(), Some(vec![1, 2]));
        assert_eq!(m.positions.len(), 6);
        for (t, tri) in m.indices.chunks(3).enumerate() {
            assert!(tri.iter().all(|&i| close(m.normals[i as usize], face_n[t])));
        }
        // Group 0 and a missing smoothing chunk mean flat shading.
        assert_eq!(folded(verts.clone(), faces.clone(), Some(vec![0, 0])).positions.len(), 6);
        assert_eq!(folded(verts.clone(), faces.clone(), None).positions.len(), 6);

        // Intersecting masks: shared vertices get one area-weighted normal.
        let m = folded(verts, faces, Some(vec![1, 3]));
        assert_eq!(m.positions.len(), 4);
        assert_eq!(m.indices, vec![0, 1, 2, 0, 2, 3]);
        let avg = Vec3::new(1.0, -1.0, 2.0).normalize();
        assert!(close(m.normals[0], avg) && close(m.normals[2], avg));
        assert!(close(m.normals[1], face_n[0]) && close(m.normals[3], face_n[1]));

        // Vertices split for a UV seam still smooth together by position.
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 1.0]];
        let m = folded(verts, vec![[0, 1, 2], [3, 4, 5]], Some(vec![1, 1]));
        assert_eq!(m.positions.len(), 6);
        assert!(close(m.normals[0], avg) && close(m.normals[3], avg));
    }

    #[test]
    fn mirrored_object_flips_winding() {
        let obj = Obj {
            name: "M",
            verts: QUAD[..3].to_vec(),
            faces: vec![[0, 1, 2]],
            smoothing: Some(vec![1]),
            matrix: Some([-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            ..Default::default()
        };
        let s = parse_3ds(&file(&[obj.bytes()]), None).unwrap();
        let m = &s.meshes[0];
        let p = |i: usize| Vec3::from(m.positions[m.indices[i] as usize]);
        assert!((p(1) - p(0)).cross(p(2) - p(0)).z < 0.0);
        assert!(m.normals.iter().all(|&n| close(n, -Vec3::Z)));
    }

    #[test]
    fn faces_split_per_material_group() {
        let obj = Obj {
            name: "Box",
            verts: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [2.0, 0.0, 0.0]],
            faces: vec![[0, 1, 2], [0, 2, 3], [1, 4, 2]],
            smoothing: Some(vec![1, 1, 1]),
            groups: vec![("Red", vec![0]), ("Blue", vec![1, 99]), ("Ghost", vec![])],
            ..Default::default()
        };
        let s = parse_3ds(&file(&[material("Red", [255, 0, 0], None), material("Blue", [0, 0, 255], None), obj.bytes()]), None).unwrap();
        let summary: Vec<_> = s.meshes.iter().map(|m| (m.name.as_str(), m.material, m.triangle_count())).collect();
        assert_eq!(summary, vec![("Box (Red)", Some(0), 1), ("Box (Blue)", Some(1), 1), ("Box", None, 1)]);
        assert_eq!(s.meshes[0].positions.len(), 3);
        assert!(s.info.warnings.iter().any(|w| w == "unknown material: Ghost"), "{:?}", s.info.warnings);
    }

    #[test]
    fn material_transparency_two_sided_and_linear_colour_preference() {
        let body = [
            chunk(MAT_NAME, &[&cstr("Glass")]),
            chunk(MAT_DIFFUSE, &[&chunk(COLOR_F, &[&f32s(&[1.0, 0.0, 0.0])]), &chunk(LIN_COLOR_F, &[&f32s(&[0.0, 0.0, 1.0])])]),
            chunk(MAT_TRANSPARENCY, &[&chunk(PERCENT_U16, &[&u16s(&[25])])]),
            chunk(MAT_TWO_SIDED, &[]),
        ]
        .concat();
        let obj =
            Obj { name: "T", verts: QUAD[..3].to_vec(), faces: vec![[0, 1, 2]], groups: vec![("Glass", vec![0])], ..Default::default() };
        let s = parse_3ds(&file(&[chunk(MATERIAL, &[&body]), obj.bytes()]), None).unwrap();
        let m = &s.materials[0];
        assert!(m.base_color.iter().zip([0.0, 0.0, 1.0, 0.75]).all(|(a, b)| (a - b).abs() < 1e-5), "{:?}", m.base_color);
        assert_eq!(m.alpha_mode, AlphaMode::Blend);
        assert!(m.double_sided);
        assert_eq!(s.meshes[0].material, Some(0));
    }

    #[test]
    fn truncated_or_corrupt_input_never_panics() {
        let d = sample();
        for cut in 0..d.len() {
            match parse_3ds(&d[..cut], None) {
                Err(_) => {}
                Ok(s) => assert!(s.info.warnings.iter().any(|w| w.contains("truncated")), "cut at {cut}: {:?}", s.info.warnings),
            }
        }
        // Overwrite every 4-byte window (lengths, counts, indices, floats) with 0xFFFFFFFF.
        for off in 0..d.len() - 4 {
            let mut c = d.clone();
            c[off..off + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let _ = parse_3ds(&c, None);
        }
    }

    #[test]
    fn rejects_non_3ds_data() {
        assert!(parse_3ds(b"", None).is_err());
        assert!(parse_3ds(b"solid cube facet normal", None).is_err());
        // Valid header but nothing inside.
        assert!(parse_3ds(&file(&[]), None).is_err());
    }
}
