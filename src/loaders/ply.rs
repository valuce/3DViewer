//! PLY loader: ASCII, binary little/big endian. Meshes and point clouds
//! (including Gaussian-splat PLYs, coloured from their DC SH term).

use crate::scene::{Material, Mesh, Scene, Topology};
use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Option<Ty> {
        Some(match s {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            _ => return None,
        })
    }
    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }
    fn is_float(self) -> bool {
        matches!(self, Ty::F32 | Ty::F64)
    }
}

#[derive(Debug)]
enum Prop {
    Scalar { name: String, ty: Ty },
    List { name: String, count: Ty, item: Ty },
}

impl Prop {
    fn name(&self) -> &str {
        match self {
            Prop::Scalar { name, .. } | Prop::List { name, .. } => name,
        }
    }
}

#[derive(Debug)]
struct Element {
    name: String,
    count: usize,
    props: Vec<Prop>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Format {
    Ascii,
    Le,
    Be,
}

struct Header {
    format: Format,
    elements: Vec<Element>,
    comments: Vec<String>,
    body_start: usize,
}

fn parse_header(data: &[u8]) -> Result<Header> {
    if !data.starts_with(b"ply") {
        bail!("not a PLY file (missing 'ply' magic)");
    }
    let end = data.windows(10).position(|w| w == b"end_header").ok_or_else(|| anyhow!("PLY header has no end_header"))?;
    let mut body_start = end + 10;
    while body_start < data.len() && data[body_start] != b'\n' {
        body_start += 1;
    }
    body_start += 1;
    let text = String::from_utf8_lossy(&data[..end]);
    let mut format = None;
    let mut elements: Vec<Element> = Vec::new();
    let mut comments = Vec::new();
    for line in text.lines().skip(1) {
        let mut t = line.split_ascii_whitespace();
        match t.next() {
            Some("format") => {
                format = Some(match t.next() {
                    Some("ascii") => Format::Ascii,
                    Some("binary_little_endian") => Format::Le,
                    Some("binary_big_endian") => Format::Be,
                    f => bail!("unknown PLY format {f:?}"),
                })
            }
            Some("comment") | Some("obj_info") => comments.push(t.collect::<Vec<_>>().join(" ")),
            Some("element") => {
                let name = t.next().unwrap_or("").to_string();
                let count = t.next().and_then(|c| c.parse().ok()).ok_or_else(|| anyhow!("bad element line: {line}"))?;
                elements.push(Element { name, count, props: Vec::new() });
            }
            Some("property") => {
                let el = elements.last_mut().ok_or_else(|| anyhow!("property before element"))?;
                let a = t.next().unwrap_or("");
                if a == "list" {
                    let count = t.next().and_then(Ty::parse).ok_or_else(|| anyhow!("bad list: {line}"))?;
                    let item = t.next().and_then(Ty::parse).ok_or_else(|| anyhow!("bad list: {line}"))?;
                    let name = t.next().unwrap_or("").to_string();
                    el.props.push(Prop::List { name, count, item });
                } else {
                    let ty = Ty::parse(a).ok_or_else(|| anyhow!("bad property type: {line}"))?;
                    let name = t.next().unwrap_or("").to_string();
                    el.props.push(Prop::Scalar { name, ty });
                }
            }
            _ => {}
        }
    }
    let format = format.ok_or_else(|| anyhow!("PLY header has no format line"))?;
    Ok(Header { format, elements, comments, body_start })
}

/// Sequential value reader over the body.
enum Body<'a> {
    Ascii(std::str::SplitAsciiWhitespace<'a>),
    Bin { data: &'a [u8], pos: usize, be: bool },
}

impl Body<'_> {
    fn read(&mut self, ty: Ty) -> Result<f64> {
        match self {
            Body::Ascii(it) => {
                let tok = it.next().ok_or_else(|| anyhow!("PLY body ended early"))?;
                tok.parse::<f64>().map_err(|_| anyhow!("bad number in PLY: {tok}"))
            }
            Body::Bin { data, pos, be } => {
                let n = ty.size();
                let b = data.get(*pos..*pos + n).ok_or_else(|| anyhow!("PLY body truncated"))?;
                *pos += n;
                Ok(decode(b, ty, *be))
            }
        }
    }
}

fn decode(b: &[u8], ty: Ty, be: bool) -> f64 {
    macro_rules! rd {
        ($t:ty, $n:expr) => {{
            let mut a = [0u8; $n];
            a.copy_from_slice(&b[..$n]);
            (if be { <$t>::from_be_bytes(a) } else { <$t>::from_le_bytes(a) }) as f64
        }};
    }
    match ty {
        Ty::I8 => b[0] as i8 as f64,
        Ty::U8 => b[0] as f64,
        Ty::I16 => rd!(i16, 2),
        Ty::U16 => rd!(u16, 2),
        Ty::I32 => rd!(i32, 4),
        Ty::U32 => rd!(u32, 4),
        Ty::F32 => rd!(f32, 4),
        Ty::F64 => rd!(f64, 8),
    }
}

fn find(props: &[Prop], names: &[&str]) -> Option<usize> {
    names.iter().find_map(|n| props.iter().position(|p| matches!(p, Prop::Scalar { .. }) && p.name() == *n))
}

fn color_scale(ty: Ty, v: f64) -> u8 {
    let x = if ty.is_float() {
        v * 255.0
    } else if matches!(ty, Ty::U16 | Ty::I16) {
        v / 257.0
    } else {
        v
    };
    x.round().clamp(0.0, 255.0) as u8
}

pub fn parse_ply(data: &[u8], dir: Option<&Path>) -> Result<Scene> {
    let h = parse_header(data)?;
    let body_bytes = data.get(h.body_start..).unwrap_or(&[]);
    let mut body = match h.format {
        Format::Ascii => Body::Ascii(std::str::from_utf8(body_bytes).context("PLY ASCII body is not UTF-8")?.split_ascii_whitespace()),
        Format::Le => Body::Bin { data: body_bytes, pos: 0, be: false },
        Format::Be => Body::Bin { data: body_bytes, pos: 0, be: true },
    };

    let mut mesh = Mesh::new("PLY", Topology::Points);
    let mut warnings = Vec::new();
    let mut has_faces = false;
    let mut vals: Vec<f64> = Vec::new();

    for el in &h.elements {
        if el.name == "vertex" {
            let p = &el.props;
            let ix = find(p, &["x"]).ok_or_else(|| anyhow!("PLY vertices have no x"))?;
            let iy = find(p, &["y"]).ok_or_else(|| anyhow!("PLY vertices have no y"))?;
            let iz = find(p, &["z"]).ok_or_else(|| anyhow!("PLY vertices have no z"))?;
            let n = [find(p, &["nx", "normal_x"]), find(p, &["ny", "normal_y"]), find(p, &["nz", "normal_z"])];
            let c =
                [find(p, &["red", "r", "diffuse_red"]), find(p, &["green", "g", "diffuse_green"]), find(p, &["blue", "b", "diffuse_blue"])];
            let ca = find(p, &["alpha", "a", "diffuse_alpha"]);
            let dc = [find(p, &["f_dc_0"]), find(p, &["f_dc_1"]), find(p, &["f_dc_2"])];
            let uv = [find(p, &["u", "s", "texture_u", "texture_s"]), find(p, &["v", "t", "texture_v", "texture_t"])];
            let has_n = n.iter().all(Option::is_some);
            let has_c = c.iter().all(Option::is_some);
            let has_dc = !has_c && dc.iter().all(Option::is_some);
            let has_uv = uv.iter().all(Option::is_some);
            let ty_of = |i: usize| match &p[i] {
                Prop::Scalar { ty, .. } => *ty,
                Prop::List { item, .. } => *item,
            };
            mesh.positions.reserve(el.count);
            for _ in 0..el.count {
                vals.clear();
                for prop in p {
                    match prop {
                        Prop::Scalar { ty, .. } => vals.push(body.read(*ty)?),
                        Prop::List { count, item, .. } => {
                            let k = body.read(*count)? as usize;
                            for _ in 0..k {
                                body.read(*item)?;
                            }
                            vals.push(0.0);
                        }
                    }
                }
                mesh.positions.push([vals[ix] as f32, vals[iy] as f32, vals[iz] as f32]);
                if has_n {
                    mesh.normals.push([vals[n[0].unwrap()] as f32, vals[n[1].unwrap()] as f32, vals[n[2].unwrap()] as f32]);
                }
                if has_c {
                    let a = ca.map(|i| color_scale(ty_of(i), vals[i])).unwrap_or(255);
                    mesh.colors.push([
                        color_scale(ty_of(c[0].unwrap()), vals[c[0].unwrap()]),
                        color_scale(ty_of(c[1].unwrap()), vals[c[1].unwrap()]),
                        color_scale(ty_of(c[2].unwrap()), vals[c[2].unwrap()]),
                        a,
                    ]);
                } else if has_dc {
                    const SH_C0: f64 = 0.282_094_79;
                    let f = |i: Option<usize>| ((0.5 + SH_C0 * vals[i.unwrap()]) * 255.0).round().clamp(0.0, 255.0) as u8;
                    mesh.colors.push([f(dc[0]), f(dc[1]), f(dc[2]), 255]);
                }
                if has_uv {
                    mesh.uvs.push([vals[uv[0].unwrap()] as f32, 1.0 - vals[uv[1].unwrap()] as f32]);
                }
            }
            mesh.had_normals = has_n;
            if has_dc {
                warnings.push("Gaussian splat PLY: shown as coloured points".to_string());
            }
        } else if el.name == "face" {
            has_faces = el.count > 0;
            let li = el
                .props
                .iter()
                .position(|p| matches!(p, Prop::List { .. }) && matches!(p.name(), "vertex_indices" | "vertex_index"))
                .or_else(|| el.props.iter().position(|p| matches!(p, Prop::List { .. })));
            if el.props.iter().any(|p| p.name() == "texcoord") {
                warnings.push("per-face texcoords are not supported".to_string());
            }
            mesh.indices.reserve(el.count * 3);
            let mut poly: Vec<u32> = Vec::with_capacity(8);
            for _ in 0..el.count {
                for (pi, prop) in el.props.iter().enumerate() {
                    match prop {
                        Prop::Scalar { ty, .. } => {
                            body.read(*ty)?;
                        }
                        Prop::List { count, item, .. } => {
                            let k = body.read(*count)? as usize;
                            poly.clear();
                            for _ in 0..k {
                                poly.push(body.read(*item)? as u32);
                            }
                            if Some(pi) == li {
                                for j in 1..k.saturating_sub(1) {
                                    mesh.indices.extend_from_slice(&[poly[0], poly[j], poly[j + 1]]);
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Skip unknown elements (edges, materials, ...).
            for _ in 0..el.count {
                for prop in &el.props {
                    match prop {
                        Prop::Scalar { ty, .. } => {
                            body.read(*ty)?;
                        }
                        Prop::List { count, item, .. } => {
                            let k = body.read(*count)? as usize;
                            for _ in 0..k {
                                body.read(*item)?;
                            }
                        }
                    }
                }
            }
        }
    }

    if mesh.positions.is_empty() {
        bail!("PLY file has no vertices");
    }
    if has_faces {
        mesh.topology = Topology::Triangles;
    } else {
        mesh.indices.clear();
    }

    let mut scene = Scene::default();
    scene.info.format = format!(
        "PLY ({})",
        match h.format {
            Format::Ascii => "ASCII",
            Format::Le => "binary LE",
            Format::Be => "binary BE",
        }
    );
    // Texture referenced by MeshLab-style comment.
    if let Some(tex) = h.comments.iter().find_map(|c| c.strip_prefix("TextureFile ")) {
        let tex = tex.trim();
        if mesh.uvs.is_empty() {
            warnings.push(format!("texture {tex} referenced but vertices have no UVs"));
        } else if let Some(d) = dir {
            match std::fs::read(d.join(tex)) {
                Ok(bytes) => match super::image::decode(tex, &bytes) {
                    Ok(t) => {
                        scene.textures.push(t);
                        scene.materials.push(Material {
                            name: tex.to_string(),
                            base_color: [1.0; 4],
                            base_color_tex: Some(0),
                            ..Default::default()
                        });
                        mesh.material = Some(0);
                    }
                    Err(e) => warnings.push(format!("texture {tex}: {e}")),
                },
                Err(e) => warnings.push(format!("texture {tex}: {e}")),
            }
        }
    }
    for c in &h.comments {
        if !c.starts_with("TextureFile") && !c.is_empty() {
            scene.info.extra.push(("comment".into(), c.clone()));
        }
    }
    scene.info.warnings = warnings;
    scene.meshes.push(mesh);
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = "ply\nformat ascii 1.0\ncomment made by hand\nelement vertex 8\nproperty float x\nproperty float y\nproperty float z\nelement face 6\nproperty list uchar int vertex_indices\nend_header\n\
0 0 0\n1 0 0\n1 1 0\n0 1 0\n0 0 1\n1 0 1\n1 1 1\n0 1 1\n\
4 0 3 2 1\n4 4 5 6 7\n4 0 1 5 4\n4 1 2 6 5\n4 2 3 7 6\n4 3 0 4 7\n";

    #[test]
    fn ascii_cube_without_normals() {
        let mut s = parse_ply(CUBE.as_bytes(), None).unwrap();
        s.finalize();
        let m = &s.meshes[0];
        assert_eq!(m.positions.len(), 8);
        assert_eq!(m.triangle_count(), 12);
        assert_eq!(m.normals.len(), 8);
        assert!(!m.had_normals);
        assert_eq!(s.info.extra[0].1, "made by hand");
    }

    #[test]
    fn binary_le_points_with_colors() {
        let mut d = b"ply\nformat binary_little_endian 1.0\nelement vertex 2\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n".to_vec();
        for (p, c) in [([1.0f32, 2.0, 3.0], [255u8, 0, 0]), ([4.0, 5.0, 6.0], [0, 255, 0])] {
            for v in p {
                d.extend_from_slice(&v.to_le_bytes());
            }
            d.extend_from_slice(&c);
        }
        let s = parse_ply(&d, None).unwrap();
        let m = &s.meshes[0];
        assert_eq!(m.topology, Topology::Points);
        assert_eq!(m.positions[1], [4.0, 5.0, 6.0]);
        assert_eq!(m.colors[0], [255, 0, 0, 255]);
        assert_eq!(s.info.format, "PLY (binary LE)");
    }

    #[test]
    fn binary_be_triangle() {
        let mut d = b"ply\nformat binary_big_endian 1.0\nelement vertex 3\nproperty double x\nproperty double y\nproperty double z\nelement face 1\nproperty list uchar uint vertex_indices\nend_header\n".to_vec();
        for p in [[0.0f64, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for v in p {
                d.extend_from_slice(&v.to_be_bytes());
            }
        }
        d.push(3);
        for i in [0u32, 1, 2] {
            d.extend_from_slice(&i.to_be_bytes());
        }
        let s = parse_ply(&d, None).unwrap();
        assert_eq!(s.meshes[0].indices, vec![0, 1, 2]);
    }

    #[test]
    fn truncated_body_is_error() {
        let d = b"ply\nformat binary_little_endian 1.0\nelement vertex 2\nproperty float x\nproperty float y\nproperty float z\nend_header\n\0\0\0\0";
        assert!(parse_ply(d, None).is_err());
    }

    #[test]
    fn garbage_is_error() {
        assert!(parse_ply(b"hello world", None).is_err());
    }
}
