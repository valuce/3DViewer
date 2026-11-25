//! Draco decoding for KHR_draco_mesh_compression (placeholder until the decoder lands).

pub struct DracoAttribute {
    pub unique_id: u32,
    pub components: usize,
    pub data: Vec<f32>,
}

pub struct DracoMesh {
    pub num_points: usize,
    pub indices: Vec<u32>,
    pub attributes: Vec<DracoAttribute>,
}

pub fn decode(_bytes: &[u8]) -> anyhow::Result<DracoMesh> {
    anyhow::bail!("Draco decoder is not available in this build")
}
