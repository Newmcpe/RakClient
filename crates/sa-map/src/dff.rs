//! Minimal RenderWare DFF *render-geometry* reader — the visual mesh, not collision.
//!
//! Some Arizona custom models ship an intentionally EMPTY collision entry (the sawmill's
//! `saw_blade`, the `obj91_*` props) or none at all, yet still have a real render mesh in their
//! `.dff`. For a viewer that only draws collision those models are invisible holes, so
//! [`crate::load`] falls back to this render mesh when a placed model has no collision geometry.
//!
//! A DFF is a tree of chunks, each `[u32 type][u32 payload size][u32 version]` + payload. We walk
//! Clump (0x10) → Geometry List (0x1A) → Geometry (0x0F) → Struct (0x01) and read the first
//! geometry's triangle list + morph-target-0 vertices. Materials/normals/UVs are skipped — only
//! positions and indices matter here.

use crate::Vec3;

const CHUNK_STRUCT: u32 = 0x01;
const CHUNK_GEOMETRY: u32 = 0x0F;
const CHUNK_CLUMP: u32 = 0x10;
const CHUNK_GEOMETRY_LIST: u32 = 0x1A;

/// Parsed render mesh: positions plus triangle index triples.
pub struct RenderMesh {
    pub vertices: Vec<Vec3>,
    pub faces: Vec<[u16; 3]>,
}

/// Extract the first geometry's render mesh from a DFF blob, or `None` if the stream has no
/// parsable geometry (not a DFF, or an empty/atomic-only clump).
pub fn parse_render_mesh(blob: &[u8]) -> Option<RenderMesh> {
    find_geometry_struct(blob, 0)
}

/// Depth-first chunk walk: descend into containers until a Geometry's Struct payload is found.
/// `depth` guards against adversarial nesting.
fn find_geometry_struct(data: &[u8], depth: u8) -> Option<RenderMesh> {
    if depth > 8 {
        return None;
    }
    let mut off = 0usize;
    while off + 12 <= data.len() {
        let ty = u32::from_le_bytes(data[off..off + 4].try_into().ok()?);
        let size = u32::from_le_bytes(data[off + 4..off + 8].try_into().ok()?) as usize;
        let payload_start = off + 12;
        let payload_end = payload_start.checked_add(size)?;
        if payload_end > data.len() {
            return None;
        }
        let payload = &data[payload_start..payload_end];
        match ty {
            CHUNK_CLUMP | CHUNK_GEOMETRY_LIST => {
                if let Some(m) = find_geometry_struct(payload, depth + 1) {
                    return Some(m);
                }
            }
            // A Geometry's first child is its Struct; parse it directly.
            CHUNK_GEOMETRY
                if payload.len() > 12
                    && u32::from_le_bytes(payload[0..4].try_into().ok()?) == CHUNK_STRUCT =>
            {
                let ssize = u32::from_le_bytes(payload[4..8].try_into().ok()?) as usize;
                if 12 + ssize <= payload.len() {
                    if let Some(m) = parse_geometry_struct(&payload[12..12 + ssize]) {
                        return Some(m);
                    }
                }
            }
            _ => {}
        }
        off = payload_end;
    }
    None
}

/// Parse a Geometry Struct payload: header, optional prelit/UV arrays, triangles, then
/// morph-target-0 vertex positions.
fn parse_geometry_struct(d: &[u8]) -> Option<RenderMesh> {
    let u32_at =
        |o: usize| -> Option<u32> { Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?)) };
    let format = u32_at(0)?;
    let num_tris = u32_at(4)? as usize;
    let num_verts = u32_at(8)? as usize;
    let _num_morphs = u32_at(12)?;
    if num_tris == 0 || num_verts == 0 || num_tris > 200_000 || num_verts > 200_000 {
        return None;
    }
    let mut off = 16usize;
    // Prelit vertex colors (format flag 0x08): RGBA per vertex.
    if format & 0x08 != 0 {
        off += num_verts * 4;
    }
    // UV sets: explicit count in bits 16..24, else inferred from TEXTURED/TEXTURED2 flags.
    let mut uv_sets = ((format >> 16) & 0xFF) as usize;
    if uv_sets == 0 {
        if format & 0x80 != 0 {
            uv_sets = 2;
        } else if format & 0x04 != 0 {
            uv_sets = 1;
        }
    }
    off += num_verts * 8 * uv_sets;
    // Triangles: [u16 v2][u16 v1][u16 material][u16 v3].
    let mut faces = Vec::with_capacity(num_tris);
    for _ in 0..num_tris {
        let t = d.get(off..off + 8)?;
        let b = u16::from_le_bytes([t[0], t[1]]);
        let a = u16::from_le_bytes([t[2], t[3]]);
        let c = u16::from_le_bytes([t[6], t[7]]);
        faces.push([a, b, c]);
        off += 8;
    }
    // Morph target 0: bounding sphere (16B), hasVertices, hasNormals, then positions.
    off += 16;
    let has_verts = u32_at(off)?;
    let _has_normals = u32_at(off + 4)?;
    off += 8;
    if has_verts == 0 {
        return None;
    }
    let mut vertices = Vec::with_capacity(num_verts);
    for _ in 0..num_verts {
        let v = d.get(off..off + 12)?;
        vertices.push(Vec3::new(
            f32::from_le_bytes(v[0..4].try_into().ok()?),
            f32::from_le_bytes(v[4..8].try_into().ok()?),
            f32::from_le_bytes(v[8..12].try_into().ok()?),
        ));
        off += 12;
    }
    // Drop faces with out-of-range indices rather than failing the whole mesh.
    faces.retain(|f| f.iter().all(|&i| (i as usize) < num_verts));
    if faces.is_empty() {
        return None;
    }
    Some(RenderMesh { vertices, faces })
}
