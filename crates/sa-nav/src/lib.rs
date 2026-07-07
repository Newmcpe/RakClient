//! Navmesh generation + storage for the SA world (SA-space public types); see docs/memory/sa-nav/lib.md#module-overview

use std::io::{Read, Write};

pub mod path;
pub use path::NavQuery;

/// SA (Z-up) -> Recast/Bevy (Y-up), handedness-preserving.
pub fn sa_to_recast(v: [f32; 3]) -> [f32; 3] {
    [v[0], v[2], -v[1]]
}

/// Recast/Bevy (Y-up) -> SA (Z-up).
pub fn recast_to_sa(v: [f32; 3]) -> [f32; 3] {
    [v[0], -v[2], v[1]]
}

/// Build parameters for a SA-MP on-foot player agent; see docs/memory/sa-nav/lib.md#onfoot-config
#[cfg(feature = "build")]
pub fn onfoot_config() -> navmesh_recast::BuildConfig {
    navmesh_recast::BuildConfig {
        cell_size: 0.25,
        cell_height: 0.2,
        walkable_slope_angle: 45.0,
        agent_height: 1.8,
        agent_radius: 0.35,
        agent_max_climb: 0.9,
        edge_max_len: 12.0,
        edge_max_error: 1.3,
        region_min_size: 8.0,
        region_merge_size: 20.0,
        detail_sample_dist: 6.0,
        detail_sample_max_error: 1.0,
    }
}

/// A navmesh in SA space (the `.nav` artifact `navgen` writes and the bot loads): same shape as [`PolyNavmesh`] with coordinates converted back to SA.
#[derive(Clone, Debug, Default)]
pub struct NavMesh {
    pub verts: Vec<[f32; 3]>,
    /// Convex polys: vertex indices + per-edge neighbour poly (-1 = solid edge).
    pub polys: Vec<NavPoly>,
    pub detail_verts: Vec<[f32; 3]>,
    pub detail_tris: Vec<[u32; 3]>,
    /// Poly i's `[first, count]` slice of `detail_tris`.
    pub detail_meshes: Vec<[u32; 2]>,
}

#[derive(Clone, Debug)]
pub struct NavPoly {
    pub verts: Vec<u32>,
    pub neighbors: Vec<i32>,
    pub area: u8,
}

impl NavMesh {
    /// Convert a Y-up recast build result into SA space.
    #[cfg(feature = "build")]
    pub fn from_recast(nav: &navmesh_recast::PolyNavmesh) -> Self {
        NavMesh {
            verts: nav.verts.iter().map(|&v| recast_to_sa(v)).collect(),
            polys: nav
                .polys
                .iter()
                .map(|p| NavPoly {
                    verts: p.verts.clone(),
                    neighbors: p.neighbors.clone(),
                    area: p.area,
                })
                .collect(),
            detail_verts: nav.detail_verts.iter().map(|&v| recast_to_sa(v)).collect(),
            detail_tris: nav.detail_tris.clone(),
            detail_meshes: nav.detail_meshes.clone(),
        }
    }

    /// Serialize to the compact `.nav` format (little-endian, magic `SANAV\x01`).
    pub fn save<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b"SANAV\x01")?;
        let put_u32 = |w: &mut W, v: u32| w.write_all(&v.to_le_bytes());
        put_u32(w, self.verts.len() as u32)?;
        put_u32(w, self.polys.len() as u32)?;
        put_u32(w, self.detail_verts.len() as u32)?;
        put_u32(w, self.detail_tris.len() as u32)?;
        for v in &self.verts {
            for f in v {
                w.write_all(&f.to_le_bytes())?;
            }
        }
        for p in &self.polys {
            w.write_all(&[p.verts.len() as u8, p.area])?;
            for &vi in &p.verts {
                put_u32(w, vi)?;
            }
            for &nb in &p.neighbors {
                w.write_all(&nb.to_le_bytes())?;
            }
        }
        for v in &self.detail_verts {
            for f in v {
                w.write_all(&f.to_le_bytes())?;
            }
        }
        for t in &self.detail_tris {
            for &i in t {
                put_u32(w, i)?;
            }
        }
        for m in &self.detail_meshes {
            put_u32(w, m[0])?;
            put_u32(w, m[1])?;
        }
        Ok(())
    }

    /// Load a `.nav` written by [`NavMesh::save`].
    pub fn load<R: Read>(r: &mut R) -> std::io::Result<Self> {
        let mut magic = [0u8; 6];
        r.read_exact(&mut magic)?;
        if &magic != b"SANAV\x01" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not a SANAV v1 file",
            ));
        }
        let mut b4 = [0u8; 4];
        let mut get_u32 = |r: &mut R| -> std::io::Result<u32> {
            r.read_exact(&mut b4)?;
            Ok(u32::from_le_bytes(b4))
        };
        let nverts = get_u32(r)? as usize;
        let npolys = get_u32(r)? as usize;
        let ndverts = get_u32(r)? as usize;
        let ndtris = get_u32(r)? as usize;
        let read_f32 = |r: &mut R| -> std::io::Result<f32> {
            let mut b = [0u8; 4];
            r.read_exact(&mut b)?;
            Ok(f32::from_le_bytes(b))
        };
        let mut verts = Vec::with_capacity(nverts);
        for _ in 0..nverts {
            verts.push([read_f32(r)?, read_f32(r)?, read_f32(r)?]);
        }
        let mut polys = Vec::with_capacity(npolys);
        for _ in 0..npolys {
            let mut hdr = [0u8; 2];
            r.read_exact(&mut hdr)?;
            let (n, area) = (hdr[0] as usize, hdr[1]);
            let mut pv = Vec::with_capacity(n);
            let mut b = [0u8; 4];
            for _ in 0..n {
                r.read_exact(&mut b)?;
                pv.push(u32::from_le_bytes(b));
            }
            let mut nb = Vec::with_capacity(n);
            for _ in 0..n {
                r.read_exact(&mut b)?;
                nb.push(i32::from_le_bytes(b));
            }
            polys.push(NavPoly {
                verts: pv,
                neighbors: nb,
                area,
            });
        }
        let mut detail_verts = Vec::with_capacity(ndverts);
        for _ in 0..ndverts {
            detail_verts.push([read_f32(r)?, read_f32(r)?, read_f32(r)?]);
        }
        let mut detail_tris = Vec::with_capacity(ndtris);
        let mut b = [0u8; 4];
        for _ in 0..ndtris {
            let mut t = [0u32; 3];
            for ti in &mut t {
                r.read_exact(&mut b)?;
                *ti = u32::from_le_bytes(b);
            }
            detail_tris.push(t);
        }
        let mut detail_meshes = Vec::with_capacity(npolys);
        for _ in 0..npolys {
            r.read_exact(&mut b)?;
            let first = u32::from_le_bytes(b);
            r.read_exact(&mut b)?;
            let count = u32::from_le_bytes(b);
            detail_meshes.push([first, count]);
        }
        Ok(NavMesh {
            verts,
            polys,
            detail_verts,
            detail_tris,
            detail_meshes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_roundtrip() {
        let v = [-506.0, -190.5, 78.2];
        assert_eq!(recast_to_sa(sa_to_recast(v)), v);
        // Handedness: SA +z (up) must land on recast +y (up).
        assert_eq!(sa_to_recast([0.0, 0.0, 1.0]), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn nav_save_load_roundtrip() {
        let nav = NavMesh {
            verts: vec![[0.0, 1.0, 2.0], [3.0, 4.0, 5.0], [6.0, 7.0, 8.0]],
            polys: vec![NavPoly {
                verts: vec![0, 1, 2],
                neighbors: vec![-1, 0, -1],
                area: 63,
            }],
            detail_verts: vec![[0.5, 1.5, 2.5]],
            detail_tris: vec![[0, 0, 0]],
            detail_meshes: vec![[0, 1]],
        };
        let mut buf = Vec::new();
        nav.save(&mut buf).unwrap();
        let back = NavMesh::load(&mut buf.as_slice()).unwrap();
        assert_eq!(back.verts, nav.verts);
        assert_eq!(back.polys.len(), 1);
        assert_eq!(back.polys[0].verts, vec![0, 1, 2]);
        assert_eq!(back.polys[0].neighbors, vec![-1, 0, -1]);
        assert_eq!(back.polys[0].area, 63);
        assert_eq!(back.detail_verts, nav.detail_verts);
        assert_eq!(back.detail_tris, nav.detail_tris);
        assert_eq!(back.detail_meshes, nav.detail_meshes);
    }
}
