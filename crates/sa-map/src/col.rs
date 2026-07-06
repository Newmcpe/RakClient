//! `COL2` / `COL3` collision parser — the physical mesh + primitive volumes for a model.
//!
//! A `.col` blob (an IMG entry) holds a run of collision models back to back; each is:
//! ```text
//!   0    char[4]  "COL2" | "COL3"
//!   4    u32      file size (bytes after this field → next model at +8+size)
//!   8    char[22] model name
//!   30   i16      model id
//!   32   TBounds  min:vec3, max:vec3, center:vec3, radius:f32     (40 bytes)
//!   72   u16      sphere count
//!   74   u16      box count
//!   76   u16      face count
//!   78   u8       line count
//!   79   u8       padding
//!   80   u32      flags
//!   84   u32      offset: spheres      ┐ all offsets are relative to +4
//!   88   u32      offset: boxes        │ (i.e. absolute = model_start + 4 + offset)
//!   92   u32      offset: lines        │
//!   96   u32      offset: vertices     │
//!   100  u32      offset: faces        │
//!   104  u32      offset: tri planes   ┘
//!   [COL3 adds shadow-mesh count + 2 offsets at 108..120]
//! ```
//! Vertices are fixed-point `int16 / 128` (so meshes span ±255.99 u); faces are three `u16` indices
//! plus a material and a light byte. The vertex *count* isn't stored — it's derived from the gap
//! between the vertex and face sections, which are contiguous in every real file (OpenRW does the
//! same). Spheres/boxes let the engine collide big simple shapes fast; the mesh is the detail.

use crate::{fixed_name, Error, Reader, Result, Vec3};

const COL2_HEADER: usize = 108;

/// A collision sphere: a fast bounding volume the engine tests before the mesh.
#[derive(Debug, Clone, Copy)]
pub struct ColSphere {
    pub center: Vec3,
    pub radius: f32,
    pub material: u8,
}

/// An axis-aligned collision box.
#[derive(Debug, Clone, Copy)]
pub struct ColBox {
    pub min: Vec3,
    pub max: Vec3,
    pub material: u8,
}

/// A mesh triangle: indices into [`ColModel::vertices`] plus its surface material.
#[derive(Debug, Clone, Copy)]
pub struct ColTriangle {
    pub a: u16,
    pub b: u16,
    pub c: u16,
    pub material: u8,
}

/// One model's collision: bounding volume, primitive shapes, and the triangle mesh. All coordinates
/// are in the model's local space — the placement layer transforms them into the world.
#[derive(Debug, Clone)]
pub struct ColModel {
    pub name: String,
    pub model_id: i16,
    /// 2 for `COL2`, 3 for `COL3`.
    pub version: u8,
    pub bound_min: Vec3,
    pub bound_max: Vec3,
    pub bound_center: Vec3,
    pub bound_radius: f32,
    pub spheres: Vec<ColSphere>,
    pub boxes: Vec<ColBox>,
    pub vertices: Vec<Vec3>,
    pub faces: Vec<ColTriangle>,
}

impl ColModel {
    /// The mesh as resolved world-local triangles, skipping any face with an out-of-range index.
    pub fn triangles(&self) -> impl Iterator<Item = [Vec3; 3]> + '_ {
        self.faces.iter().filter_map(move |f| {
            let a = self.vertices.get(f.a as usize)?;
            let b = self.vertices.get(f.b as usize)?;
            let c = self.vertices.get(f.c as usize)?;
            Some([*a, *b, *c])
        })
    }
}

/// Parse every collision model in a `.col` blob (an IMG entry may pack many). Stops cleanly at the
/// first non-`COL*` FourCC (trailing sector padding is zeros, not a model).
pub fn parse_archive(data: &[u8]) -> Result<Vec<ColModel>> {
    let mut models = Vec::new();
    let mut start = 0usize;
    while start + 8 <= data.len() {
        let fourcc = &data[start..start + 4];
        if !matches!(fourcc, b"COL2" | b"COL3") {
            break; // padding or an unsupported version — end of the run
        }
        let (model, next) = parse_one(data, start)?;
        models.push(model);
        if next <= start {
            break; // malformed size field; don't spin
        }
        start = next;
    }
    Ok(models)
}

/// Parse the single collision model beginning at `start`; return it and the offset of the next model.
fn parse_one(data: &[u8], start: usize) -> Result<(ColModel, usize)> {
    let mut r = Reader::at(data, start);
    let magic = r.magic()?;
    let version = match &magic {
        b"COL2" => 2,
        b"COL3" => 3,
        other => return Err(Error::UnsupportedCol(*other)),
    };
    let file_size = r.u32()? as usize;
    let next = start + 8 + file_size; // the size counts bytes after this field
    let name = fixed_name(r.bytes(22)?);
    let model_id = r.i16()?;
    let bound_min = r.vec3()?;
    let bound_max = r.vec3()?;
    let bound_center = r.vec3()?;
    let bound_radius = r.f32()?;

    let num_spheres = r.u16()? as usize;
    let num_boxes = r.u16()? as usize;
    let num_faces = r.u16()? as usize;
    let _num_lines = r.u8()?;
    let _pad = r.u8()?;
    let _flags = r.u32()?;
    // Offsets are relative to the byte after the FourCC.
    let base = start + 4;
    let off_spheres = base + r.u32()? as usize;
    let off_boxes = base + r.u32()? as usize;
    let _off_lines = base + r.u32()? as usize;
    let off_verts_rel = r.u32()? as usize;
    let off_faces_rel = r.u32()? as usize;
    let _off_planes = r.u32()?;
    debug_assert!(r.pos() <= start + COL2_HEADER + if version == 3 { 12 } else { 0 });

    let mut spheres = Vec::with_capacity(num_spheres);
    let mut sr = Reader::at(data, off_spheres);
    for _ in 0..num_spheres {
        let center = sr.vec3()?;
        let radius = sr.f32()?;
        let material = sr.u8()?;
        let _flag = sr.u8()?;
        let _brightness = sr.u8()?;
        let _light = sr.u8()?;
        spheres.push(ColSphere {
            center,
            radius,
            material,
        });
    }

    let mut boxes = Vec::with_capacity(num_boxes);
    let mut br = Reader::at(data, off_boxes);
    for _ in 0..num_boxes {
        let min = br.vec3()?;
        let max = br.vec3()?;
        let material = br.u8()?;
        let _flag = br.u8()?;
        let _brightness = br.u8()?;
        let _light = br.u8()?;
        boxes.push(ColBox { min, max, material });
    }

    // Mesh: the vertex count is implied by the vertex→face gap (the two sections are contiguous).
    let mut vertices = Vec::new();
    let mut faces = Vec::new();
    if num_faces > 0 && off_verts_rel > 0 && off_faces_rel > off_verts_rel {
        let num_verts = (off_faces_rel - off_verts_rel) / 6;
        let mut vr = Reader::at(data, base + off_verts_rel);
        vertices.reserve(num_verts);
        for _ in 0..num_verts {
            let x = vr.i16()? as f32 / 128.0;
            let y = vr.i16()? as f32 / 128.0;
            let z = vr.i16()? as f32 / 128.0;
            vertices.push(Vec3::new(x, y, z));
        }
        let mut fr = Reader::at(data, base + off_faces_rel);
        faces.reserve(num_faces);
        for _ in 0..num_faces {
            let a = fr.u16()?;
            let b = fr.u16()?;
            let c = fr.u16()?;
            let material = fr.u8()?;
            let _light = fr.u8()?;
            faces.push(ColTriangle { a, b, c, material });
        }
    }

    Ok((
        ColModel {
            name,
            model_id,
            version,
            bound_min,
            bound_max,
            bound_center,
            bound_radius,
            spheres,
            boxes,
            vertices,
            faces,
        },
        next,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-build a byte-exact COL2 with one sphere, one box and a 4-vertex / 2-face mesh, with the
    /// sections laid out contiguously (spheres, boxes, vertices, faces) exactly as real files are.
    fn golden_col2() -> Vec<u8> {
        // Section byte offsets (absolute). Header is 108 bytes; offset fields are relative to +4.
        const HEADER: usize = 108;
        let sphere_at = HEADER; // 108, len 20 → 128
        let box_at = sphere_at + 20; // 128, len 28 → 156
        let vert_at = box_at + 28; // 156, len 4*6=24 → 180
        let face_at = vert_at + 24; // 180, len 2*8=16 → 196
        let total = face_at + 16;

        let mut b = vec![0u8; total];
        b[0..4].copy_from_slice(b"COL2");
        let file_size = (total - 8) as u32;
        b[4..8].copy_from_slice(&file_size.to_le_bytes());
        b[8..12].copy_from_slice(b"tst\0");
        b[30..32].copy_from_slice(&1337i16.to_le_bytes());
        // TBounds min/max/center/radius (32..72) — values chosen distinct so a mix-up would show.
        let put_vec = |b: &mut [u8], at: usize, v: [f32; 3]| {
            b[at..at + 4].copy_from_slice(&v[0].to_le_bytes());
            b[at + 4..at + 8].copy_from_slice(&v[1].to_le_bytes());
            b[at + 8..at + 12].copy_from_slice(&v[2].to_le_bytes());
        };
        put_vec(&mut b, 32, [-1.0, -2.0, -3.0]); // min
        put_vec(&mut b, 44, [1.0, 2.0, 3.0]); // max
        put_vec(&mut b, 56, [0.0, 0.0, 0.5]); // center
        b[68..72].copy_from_slice(&4.0f32.to_le_bytes()); // radius
        b[72..74].copy_from_slice(&1u16.to_le_bytes()); // spheres
        b[74..76].copy_from_slice(&1u16.to_le_bytes()); // boxes
        b[76..78].copy_from_slice(&2u16.to_le_bytes()); // faces
                                                        // lines(78)=0, pad(79)=0, flags(80..84)=0
        let base = 4;
        b[84..88].copy_from_slice(&((sphere_at - base) as u32).to_le_bytes());
        b[88..92].copy_from_slice(&((box_at - base) as u32).to_le_bytes());
        b[92..96].copy_from_slice(&0u32.to_le_bytes()); // lines
        b[96..100].copy_from_slice(&((vert_at - base) as u32).to_le_bytes());
        b[100..104].copy_from_slice(&((face_at - base) as u32).to_le_bytes());
        b[104..108].copy_from_slice(&0u32.to_le_bytes()); // planes

        // Sphere: center (5,6,7), radius 8, material 3.
        put_vec(&mut b, sphere_at, [5.0, 6.0, 7.0]);
        b[sphere_at + 12..sphere_at + 16].copy_from_slice(&8.0f32.to_le_bytes());
        b[sphere_at + 16] = 3;
        // Box: min (-1,-1,-1) max (1,1,1), material 4.
        put_vec(&mut b, box_at, [-1.0, -1.0, -1.0]);
        put_vec(&mut b, box_at + 12, [1.0, 1.0, 1.0]);
        b[box_at + 24] = 4;
        // Vertices (int16 / 128): (128,256,-128)→(1,2,-1), (0,0,0), (256,0,0)→(2,0,0), (0,256,0)→(0,2,0)
        let put_v16 = |b: &mut [u8], at: usize, v: [i16; 3]| {
            b[at..at + 2].copy_from_slice(&v[0].to_le_bytes());
            b[at + 2..at + 4].copy_from_slice(&v[1].to_le_bytes());
            b[at + 4..at + 6].copy_from_slice(&v[2].to_le_bytes());
        };
        put_v16(&mut b, vert_at, [128, 256, -128]);
        put_v16(&mut b, vert_at + 6, [0, 0, 0]);
        put_v16(&mut b, vert_at + 12, [256, 0, 0]);
        put_v16(&mut b, vert_at + 18, [0, 256, 0]);
        // Faces: (0,1,2) mat 7, (1,2,3) mat 8.
        let put_face = |b: &mut [u8], at: usize, i: [u16; 3], mat: u8| {
            b[at..at + 2].copy_from_slice(&i[0].to_le_bytes());
            b[at + 2..at + 4].copy_from_slice(&i[1].to_le_bytes());
            b[at + 4..at + 6].copy_from_slice(&i[2].to_le_bytes());
            b[at + 6] = mat;
        };
        put_face(&mut b, face_at, [0, 1, 2], 7);
        put_face(&mut b, face_at + 8, [1, 2, 3], 8);
        b
    }

    #[test]
    fn parses_golden_col2() {
        let models = parse_archive(&golden_col2()).expect("parse");
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(m.name, "tst");
        assert_eq!(m.model_id, 1337);
        assert_eq!(m.version, 2);
        assert_eq!(m.bound_radius, 4.0);

        assert_eq!(m.spheres.len(), 1);
        assert_eq!(m.spheres[0].center, Vec3::new(5.0, 6.0, 7.0));
        assert_eq!(m.spheres[0].radius, 8.0);
        assert_eq!(m.spheres[0].material, 3);

        assert_eq!(m.boxes.len(), 1);
        assert_eq!(m.boxes[0].max, Vec3::new(1.0, 1.0, 1.0));

        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.vertices[0], Vec3::new(1.0, 2.0, -1.0));
        assert_eq!(m.vertices[2], Vec3::new(2.0, 0.0, 0.0));

        assert_eq!(m.faces.len(), 2);
        assert_eq!((m.faces[0].a, m.faces[0].b, m.faces[0].c), (0, 1, 2));
        assert_eq!(m.faces[1].material, 8);

        // triangles() resolves indices to coordinates.
        let tris: Vec<_> = m.triangles().collect();
        assert_eq!(tris.len(), 2);
        assert_eq!(tris[0][0], Vec3::new(1.0, 2.0, -1.0));
    }

    #[test]
    fn stops_at_padding_after_model() {
        // A model followed by a sector of zeros parses to exactly one model, not an error.
        let mut bytes = golden_col2();
        bytes.resize(bytes.len() + 2048, 0);
        assert_eq!(parse_archive(&bytes).expect("parse").len(), 1);
    }
}
