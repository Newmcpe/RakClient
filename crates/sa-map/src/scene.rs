//! Prebaked viewer scene: the fully assembled world geometry dumped to disk so
//! the Bevy viewer opens instantly instead of re-parsing every IMG + streamer
//! bin + DFF on each launch (tens of seconds of silent work → a sequential
//! binary read). Baked by `samap scene`, consumed by `sa-viewer`.
//!
//! All coordinates are SA space, exactly as `world::build`/`place_objects` emit.

use std::io::{self, Read, Write};

use crate::Mesh;

/// A baked viewer scene: the base collision world, the streamed-object overlay
/// (Arizona custom map from a pcap CSV), and the diagnostic "hole" markers.
#[derive(Debug, Default, Clone)]
pub struct Scene {
    /// The merged world collision mesh (height-coloured terrain in the viewer).
    pub base: Mesh,
    /// Server-streamed `CreateObject` overlay, rendered distinctly. Empty if no CSV.
    pub streamed: Mesh,
    /// SA positions of instances whose model placed no collision (red markers).
    pub holes: Vec<[f32; 3]>,
}

const MAGIC: &[u8; 6] = b"SASCN\x01";

impl Scene {
    /// Serialize to the compact `.scene` format (little-endian, magic `SASCN\x01`).
    pub fn save<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(MAGIC)?;
        write_mesh(w, &self.base)?;
        write_mesh(w, &self.streamed)?;
        write_u32(w, self.holes.len() as u32)?;
        for h in &self.holes {
            write_vec3(w, *h)?;
        }
        Ok(())
    }

    /// Load a `.scene` written by [`Scene::save`].
    pub fn load<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut magic = [0u8; 6];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a SASCN v1 scene file",
            ));
        }
        let base = read_mesh(r)?;
        let streamed = read_mesh(r)?;
        let nholes = read_u32(r)? as usize;
        let mut holes = Vec::with_capacity(nholes);
        for _ in 0..nholes {
            holes.push(read_vec3(r)?);
        }
        Ok(Scene {
            base,
            streamed,
            holes,
        })
    }
}

fn write_u32<W: Write>(w: &mut W, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn write_vec3<W: Write>(w: &mut W, v: [f32; 3]) -> io::Result<()> {
    for f in v {
        w.write_all(&f.to_le_bytes())?;
    }
    Ok(())
}

fn read_vec3<R: Read>(r: &mut R) -> io::Result<[f32; 3]> {
    let mut v = [0.0f32; 3];
    let mut b = [0u8; 4];
    for f in &mut v {
        r.read_exact(&mut b)?;
        *f = f32::from_le_bytes(b);
    }
    Ok(v)
}

fn write_mesh<W: Write>(w: &mut W, m: &Mesh) -> io::Result<()> {
    write_u32(w, m.positions.len() as u32)?;
    write_u32(w, m.indices.len() as u32)?;
    for p in &m.positions {
        write_vec3(w, *p)?;
    }
    for &i in &m.indices {
        write_u32(w, i)?;
    }
    Ok(())
}

fn read_mesh<R: Read>(r: &mut R) -> io::Result<Mesh> {
    let nverts = read_u32(r)? as usize;
    let nidx = read_u32(r)? as usize;
    let mut positions = Vec::with_capacity(nverts);
    for _ in 0..nverts {
        positions.push(read_vec3(r)?);
    }
    let mut indices = Vec::with_capacity(nidx);
    for _ in 0..nidx {
        indices.push(read_u32(r)?);
    }
    Ok(Mesh { positions, indices })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_roundtrip() {
        let scene = Scene {
            base: Mesh {
                positions: vec![[0.0, 1.0, 2.0], [3.0, 4.0, 5.0], [6.0, 7.0, 8.0]],
                indices: vec![0, 1, 2],
            },
            streamed: Mesh {
                positions: vec![[9.0, 9.0, 9.0]],
                indices: vec![0, 0, 0],
            },
            holes: vec![[-1.0, -2.0, -3.0], [10.0, 20.0, 30.0]],
        };
        let mut buf = Vec::new();
        scene.save(&mut buf).unwrap();
        let back = Scene::load(&mut buf.as_slice()).unwrap();
        assert_eq!(back.base.positions, scene.base.positions);
        assert_eq!(back.base.indices, scene.base.indices);
        assert_eq!(back.streamed.positions, scene.streamed.positions);
        assert_eq!(back.holes, scene.holes);
    }
}
