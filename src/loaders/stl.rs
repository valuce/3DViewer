//! STL loader (binary and ASCII). Flat shaded: three unique vertices per facet.

use super::{read_f32_le, read_u32_le};
use crate::scene::{Mesh, Scene, Topology};
use anyhow::{Result, bail};
use glam::Vec3;

pub fn parse_stl(data: &[u8]) -> Result<Scene> {
    let looks_ascii = data.trim_ascii_start().starts_with(b"solid");
    let (mesh, binary) = if is_binary(data) {
        match parse_binary(data) {
            Ok(m) => (m, true),
            Err(e) if looks_ascii => (parse_ascii(data).map_err(|_| e)?, false),
            Err(e) => return Err(e),
        }
    } else {
        (parse_ascii(data)?, false)
    };
    let mut scene = Scene::default();
    scene.info.up_axis_z = true;
    scene.info.format = if binary { "STL (binary)".into() } else { "STL (ASCII)".into() };
    scene.meshes.push(mesh);
    Ok(scene)
}

fn is_binary(data: &[u8]) -> bool {
    if data.len() >= 84 {
        let n = read_u32_le(data, 80) as u64;
        if 84 + n * 50 == data.len() as u64 {
            return true;
        }
    }
    let head = &data[..data.len().min(512)];
    let starts_solid = head.trim_ascii_start().starts_with(b"solid");
    // Some binary files start with "solid" in the header; ASCII must contain "facet".
    !(starts_solid && contains(head, b"facet") || starts_solid && data.len() < 84)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn push_facet(mesh: &mut Mesh, v: [Vec3; 3], file_n: Vec3) {
    let mut n = (v[1] - v[0]).cross(v[2] - v[0]).normalize_or_zero();
    if n == Vec3::ZERO {
        n = file_n.normalize_or_zero();
    }
    let base = mesh.positions.len() as u32;
    for p in v {
        mesh.positions.push(p.to_array());
        mesh.normals.push(n.to_array());
    }
    mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
}

fn parse_binary(data: &[u8]) -> Result<Mesh> {
    if data.len() < 84 {
        bail!("binary STL too short");
    }
    let n = read_u32_le(data, 80) as usize;
    let available = (data.len() - 84) / 50;
    if available < n {
        bail!("binary STL truncated: header says {n} triangles, file holds {available}");
    }
    let name = String::from_utf8_lossy(&data[..80]).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
    let mut mesh = Mesh::new(if name.is_empty() { "STL".to_string() } else { name }, Topology::Triangles);
    mesh.positions.reserve(n * 3);
    mesh.normals.reserve(n * 3);
    mesh.indices.reserve(n * 3);
    let mut colors = Vec::new();
    // VisCAM / SolidView colour convention: bit 15 set => valid RGB555 colour.
    let has_color = (0..n).any(|i| read_u16(data, 84 + i * 50 + 48) & 0x8000 != 0);
    for i in 0..n {
        let o = 84 + i * 50;
        let r = |k: usize| read_f32_le(data, o + k * 4);
        let fn_ = Vec3::new(r(0), r(1), r(2));
        let v = [Vec3::new(r(3), r(4), r(5)), Vec3::new(r(6), r(7), r(8)), Vec3::new(r(9), r(10), r(11))];
        push_facet(&mut mesh, v, fn_);
        if has_color {
            let a = read_u16(data, o + 48);
            let c = if a & 0x8000 != 0 {
                let f = |s: u16| (((a >> s) & 31) as u32 * 255 / 31) as u8;
                [f(0), f(5), f(10), 255]
            } else {
                [200, 200, 200, 255]
            };
            colors.extend_from_slice(&[c, c, c]);
        }
    }
    mesh.colors = colors;
    mesh.had_normals = true;
    Ok(mesh)
}

fn read_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn parse_ascii(data: &[u8]) -> Result<Mesh> {
    let text = String::from_utf8_lossy(data);
    let mut tokens = text.split_ascii_whitespace();
    let mut name = String::from("STL");
    if tokens.next() != Some("solid") {
        bail!("not an STL file (missing 'solid')");
    }
    let mut mesh = Mesh::new("", Topology::Triangles);
    let mut verts: Vec<Vec3> = Vec::with_capacity(3);
    let mut file_n = Vec3::ZERO;
    let mut first = true;
    while let Some(t) = tokens.next() {
        match t {
            "facet" => {
                verts.clear();
                if tokens.next() == Some("normal") {
                    file_n = read_vec3(&mut tokens)?;
                }
            }
            "vertex" => {
                verts.push(read_vec3(&mut tokens)?);
                if verts.len() == 3 {
                    push_facet(&mut mesh, [verts[0], verts[1], verts[2]], file_n);
                    verts.clear();
                }
            }
            "endsolid" | "outer" | "loop" | "endloop" | "endfacet" => {}
            other if first => name = other.to_string(),
            _ => {}
        }
        if t != "solid" {
            first = false;
        }
    }
    if mesh.positions.is_empty() {
        bail!("ASCII STL contains no facets");
    }
    mesh.name = name;
    mesh.had_normals = true;
    Ok(mesh)
}

fn read_vec3<'a>(it: &mut impl Iterator<Item = &'a str>) -> Result<Vec3> {
    let mut v = [0f32; 3];
    for c in &mut v {
        let tok = it.next().ok_or_else(|| anyhow::anyhow!("unexpected end of STL"))?;
        *c = tok.parse().map_err(|_| anyhow::anyhow!("bad number in STL: {tok}"))?;
    }
    Ok(Vec3::from(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn binary_stl(tris: &[[[f32; 3]; 3]]) -> Vec<u8> {
        let mut d = vec![0u8; 80];
        d.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        for t in tris {
            d.extend_from_slice(&[0u8; 12]);
            for v in t {
                for c in v {
                    d.extend_from_slice(&c.to_le_bytes());
                }
            }
            d.extend_from_slice(&[0, 0]);
        }
        d
    }

    #[test]
    fn binary_one_triangle() {
        let d = binary_stl(&[[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]);
        let s = parse_stl(&d).unwrap();
        assert_eq!(s.meshes[0].positions.len(), 3);
        assert_eq!(s.meshes[0].indices, vec![0, 1, 2]);
        assert_eq!(s.meshes[0].normals[0], [0.0, 0.0, 1.0]);
        assert!(s.info.up_axis_z);
    }

    #[test]
    fn binary_header_starting_with_solid() {
        let mut d = binary_stl(&[[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]);
        d[..5].copy_from_slice(b"solid");
        assert_eq!(parse_stl(&d).unwrap().triangle_count(), 1);
    }

    #[test]
    fn ascii_one_triangle() {
        let t = "solid cube\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 0 1 0\n  endloop\n endfacet\nendsolid cube\n";
        let s = parse_stl(t.as_bytes()).unwrap();
        assert_eq!(s.meshes[0].name, "cube");
        assert_eq!(s.triangle_count(), 1);
        assert_eq!(s.info.format, "STL (ASCII)");
    }

    #[test]
    fn truncated_binary_is_error() {
        let mut d = binary_stl(&[[[0.0; 3]; 3], [[0.0; 3]; 3]]);
        d.truncate(84 + 60);
        // header count = 2 but only one full facet
        assert!(parse_stl(&d).is_err());
    }
}
