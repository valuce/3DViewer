//! OFF loader (OFF, COFF, NOFF, CNOFF; ASCII only).

use crate::scene::{Mesh, Scene, Topology};
use anyhow::{Result, anyhow, bail};

pub fn parse_off(text: &str) -> Result<Scene> {
    let mut lines = text.lines().map(|l| l.split('#').next().unwrap_or("").trim()).filter(|l| !l.is_empty());
    let first = lines.next().ok_or_else(|| anyhow!("empty OFF file"))?;
    let mut head = first.split_ascii_whitespace();
    let kw = head.next().unwrap_or("");
    if !kw.ends_with("OFF") {
        bail!("not an OFF file (header '{kw}')");
    }
    let prefix = &kw[..kw.len() - 3];
    let has_c = prefix.contains('C');
    let has_n = prefix.contains('N');
    // Counts may follow the keyword on the same line.
    let rest: Vec<&str> = head.collect();
    let counts_line: Vec<usize> = if rest.len() >= 2 {
        rest.iter().filter_map(|t| t.parse().ok()).collect()
    } else {
        lines.next().ok_or_else(|| anyhow!("OFF missing counts"))?.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect()
    };
    if counts_line.len() < 2 {
        bail!("OFF counts line is malformed");
    }
    let (nv, nf) = (counts_line[0], counts_line[1]);

    let mut mesh = Mesh::new("OFF", if nf > 0 { Topology::Triangles } else { Topology::Points });
    mesh.positions.reserve(nv);
    for i in 0..nv {
        let l = lines.next().ok_or_else(|| anyhow!("OFF ended at vertex {i}"))?;
        let v: Vec<f32> = l.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect();
        if v.len() < 3 {
            bail!("OFF vertex {i} malformed");
        }
        mesh.positions.push([v[0], v[1], v[2]]);
        let mut k = 3;
        if has_n && v.len() >= k + 3 {
            mesh.normals.push([v[k], v[k + 1], v[k + 2]]);
            k += 3;
        }
        if has_c && v.len() >= k + 3 {
            mesh.colors.push(color(&v[k..]));
        }
    }
    let mut face_colors = false;
    for i in 0..nf {
        let l = lines.next().ok_or_else(|| anyhow!("OFF ended at face {i}"))?;
        let v: Vec<f32> = l.split_ascii_whitespace().filter_map(|t| t.parse().ok()).collect();
        let k = *v.first().ok_or_else(|| anyhow!("OFF face {i} malformed"))? as usize;
        if v.len() < 1 + k {
            bail!("OFF face {i} malformed");
        }
        face_colors |= v.len() >= 1 + k + 3;
        for j in 1..k.saturating_sub(1) {
            mesh.indices.extend_from_slice(&[v[1] as u32, v[1 + j] as u32, v[2 + j] as u32]);
        }
    }
    mesh.had_normals = mesh.normals.len() == nv && nv > 0;
    let mut scene = Scene::default();
    scene.info.format = format!("OFF{}", if prefix.is_empty() { String::new() } else { format!(" ({prefix})") });
    if face_colors {
        scene.info.warnings.push("per-face colours are ignored".into());
    }
    scene.meshes.push(mesh);
    Ok(scene)
}

fn color(v: &[f32]) -> [u8; 4] {
    let ints = v.iter().take(4).any(|&x| x > 1.0);
    let c = |x: f32| if ints { x.clamp(0.0, 255.0) as u8 } else { (x * 255.0).round().clamp(0.0, 255.0) as u8 };
    [c(v[0]), c(v[1]), c(v[2]), if v.len() > 3 { c(v[3]) } else { 255 }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangle_and_quad() {
        let t = "OFF\n# comment\n5 2 0\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n2 2 0\n4 0 1 2 3\n3 1 4 2\n";
        let s = parse_off(t).unwrap();
        assert_eq!(s.meshes[0].triangle_count(), 3);
        assert_eq!(s.meshes[0].positions.len(), 5);
    }

    #[test]
    fn coff_colors() {
        let t = "COFF\n3 1 0\n0 0 0 255 0 0 255\n1 0 0 0 255 0 255\n0 1 0 0 0 255 255\n3 0 1 2\n";
        let s = parse_off(t).unwrap();
        assert_eq!(s.meshes[0].colors[1], [0, 255, 0, 255]);
    }

    #[test]
    fn truncated_is_error() {
        assert!(parse_off("OFF\n3 1 0\n0 0 0\n").is_err());
    }
}
