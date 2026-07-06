//! Safe Rust wrapper over the canonical C++ recastnavigation build pipeline.
//! Turns a world-space triangle soup (terrain + collision) into a convex-polygon
//! 3D navmesh, filtered by tank slope / clearance / step / radius.

/// SPIKE: pure-Rust `rerecast` backend producing the same `PolyNavmesh`.
pub mod rerecast_backend;
pub use rerecast_backend::build_rerecast;

/// Milestone-0 tiled build: per-tile rasterisation merged back into one flat
/// `PolyNavmesh` with welded borders + rebuilt adjacency.
pub mod tiled;
pub use tiled::build_rerecast_tiled;

#[cfg(recast_cpp)]
use std::os::raw::{c_char, c_float, c_int, c_uchar};

#[cfg(recast_cpp)]
#[repr(C)]
struct RcConfigC {
    cell_size: c_float,
    cell_height: c_float,
    walkable_slope_angle: c_float,
    agent_height: c_float,
    agent_radius: c_float,
    agent_max_climb: c_float,
    edge_max_len: c_float,
    edge_max_error: c_float,
    region_min_size: c_float,
    region_merge_size: c_float,
    detail_sample_dist: c_float,
    detail_sample_max_error: c_float,
}

#[cfg(recast_cpp)]
#[repr(C)]
struct RcResultC {
    verts: *mut c_float,
    nverts: c_int,
    polys: *mut c_int,
    npolys: c_int,
    nvp: c_int,
    areas: *mut c_uchar,
    dverts: *mut c_float,
    ndverts: c_int,
    dtris: *mut c_int,
    ndtris: c_int,
    dmeshes: *mut c_int,
    ndmeshes: c_int,
    ok: c_int,
    err: [c_char; 128],
}

#[cfg(recast_cpp)]
extern "C" {
    fn recast_build(
        verts: *const c_float,
        nverts: c_int,
        tris: *const c_int,
        ntris: c_int,
        tri_obstacle: *const c_uchar,
        cfg: *const RcConfigC,
    ) -> *mut RcResultC;
    fn recast_free(r: *mut RcResultC);
}

/// Recast build parameters, in meters/degrees. Defaults are tuned for a WoT tank.
#[derive(Clone, Copy, Debug)]
pub struct BuildConfig {
    pub cell_size: f32,
    pub cell_height: f32,
    pub walkable_slope_angle: f32,
    pub agent_height: f32,
    pub agent_radius: f32,
    pub agent_max_climb: f32,
    pub edge_max_len: f32,
    pub edge_max_error: f32,
    pub region_min_size: f32,
    pub region_merge_size: f32,
    pub detail_sample_dist: f32,
    pub detail_sample_max_error: f32,
}

impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            cell_size: 1.0,
            cell_height: 0.4,
            // Tank physics climb limit (~25-27° sustained), not the old permissive
            // 35°: WoT bakes rock/cliff aprons into the heightmap at 25-38°, and the
            // engine's own navgen (physics girth-agent) rejects them — 35° let bots
            // onto rocky skirts the game never allows (validated on the 31_airfield
            // rock band). FUFLO_SLOPE overrides.
            walkable_slope_angle: 25.0,
            agent_height: 3.0,
            // 1.0m so Recast's ceil(radius/cell_size=1m) erodes ONE 1m voxel, not
            // two: radius 1.3..1.7 all quantize to 2 cells (2.0m bilateral = 3.4m),
            // which seals tank-width (~3m) passages obstacles already pinch. The
            // Havok hull over-extends past the visual wall, so 1m erosion keeps the
            // nav edge inside real walls (measured: no through-wall paths). Tunable
            // via FUFLO_RADIUS; for sub-cell precision shrink cell_size instead.
            agent_radius: 1.0,
            // Engine-faithful step-up = hopMax = agent_height/3 (BigWorld navgen
            // physics_handler.cpp:78; `mem:navmesh-collision-flags-source`). For
            // agent_height 3.0 that is exactly 1.0, so the default is unchanged;
            // the relationship is what matters if agent_height is retuned. The
            // Recast slope angle below has NO engine analogue (the engine uses the
            // per-step flood, not an angle) — it stays an approximation. FUFLO_CLIMB overrides.
            agent_max_climb: 1.0,
            edge_max_len: 12.0,
            edge_max_error: 1.3,
            region_min_size: 8.0,
            region_merge_size: 20.0,
            // ~1-unit detail triangulation: sample the walkable surface every
            // 1 cell (= cell_size = 1m) so the detail mesh tessellates the ground
            // into roughly 1-unit triangles that follow terrain height. The small
            // max-error keeps even gently-sloped ground subdivided (rather than
            // collapsing it back to a few big flat tris).
            detail_sample_dist: 1.0,
            detail_sample_max_error: 0.4,
        }
    }
}

/// One convex navmesh polygon: world-vertex indices + per-edge neighbour polygon
/// (-1 = border/solid edge, else adjacent polygon index). `area` is the Recast area id.
#[derive(Clone, Debug)]
pub struct Poly {
    pub verts: Vec<u32>,
    pub neighbors: Vec<i32>,
    pub area: u8,
}

/// A convex-polygon 3D navmesh in world space, plus the matching detail mesh.
///
/// `verts`/`polys` are the convex polygon mesh (flat plane fits, good adjacency).
/// `detail_verts`/`detail_tris` are a ~1-unit triangulation of the SAME surface
/// that follows the terrain height — this is the source for the runtime triangle
/// navmesh. Detail tri indices are global into `detail_verts`; boundary vertices
/// shared between adjacent polygons coincide exactly (weld to recover topology).
/// `detail_meshes[i] = [first_tri, tri_count]` is poly `i`'s contiguous slice of
/// `detail_tris` (recast emits one detail submesh per convex poly, in order), so
/// each detail triangle maps back to its parent poly's area + dense-Y queries.
#[derive(Clone, Debug, Default)]
pub struct PolyNavmesh {
    pub verts: Vec<[f32; 3]>,
    pub nvp: usize,
    pub polys: Vec<Poly>,
    pub detail_verts: Vec<[f32; 3]>,
    pub detail_tris: Vec<[u32; 3]>,
    pub detail_meshes: Vec<[u32; 2]>,
}

impl PolyNavmesh {
    /// Drop every connected component except the largest by walkable XZ area,
    /// flooding over `Poly::neighbors` — the exact adjacency the runtime A*
    /// traverses, so a dropped polygon is one no path from the main field could
    /// ever reach. Engine ground-truth: BigWorld's navgen emits waypoints only on
    /// surfaces its girth-agent physically reaches from the playable area
    /// (waypoint_flood.cpp), so enclosed rock pockets, roofs, and out-of-bounds
    /// terrain shelves get NO navmesh at all; Recast instead meshes every locally
    /// walkable surface and leaves them as unreachable islands.
    ///
    /// Area (not poly count) picks the survivor: out-of-bounds shelves fragment
    /// into many small polys and could out-count the merged-convex main field.
    ///
    /// Returns `(kept_polys, dropped_polys, dropped_components)`.
    pub fn retain_largest_component(&mut self) -> (usize, usize, usize) {
        if self.polys.is_empty() {
            return (0, 0, 0);
        }
        let poly_area_xz = |p: &Poly| -> f64 {
            let v = |i: usize| self.verts[p.verts[i] as usize];
            let mut a = 0.0f64;
            for i in 1..p.verts.len().saturating_sub(1) {
                let (o, b, c) = (v(0), v(i), v(i + 1));
                a += 0.5
                    * f64::from((b[0] - o[0]) * (c[2] - o[2]) - (b[2] - o[2]) * (c[0] - o[0]))
                        .abs();
            }
            a
        };

        let mut comp = vec![usize::MAX; self.polys.len()];
        let mut comp_area: Vec<f64> = Vec::new();
        for seed in 0..self.polys.len() {
            if comp[seed] != usize::MAX {
                continue;
            }
            let id = comp_area.len();
            let mut area = 0.0;
            let mut stack = vec![seed];
            comp[seed] = id;
            while let Some(i) = stack.pop() {
                area += poly_area_xz(&self.polys[i]);
                for &nb in &self.polys[i].neighbors {
                    let Ok(nb) = usize::try_from(nb) else {
                        continue;
                    };
                    if nb < self.polys.len() && comp[nb] == usize::MAX {
                        comp[nb] = id;
                        stack.push(nb);
                    }
                }
            }
            comp_area.push(area);
        }
        if comp_area.len() == 1 {
            return (self.polys.len(), 0, 0);
        }
        let main = comp_area
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .expect("non-empty: one component per seed poly");

        // Compact polys; remap poly indices in `neighbors` (dropped -> -1).
        let mut new_poly_idx = vec![-1i32; self.polys.len()];
        let mut kept = 0i32;
        for (i, c) in comp.iter().enumerate() {
            if *c == main {
                new_poly_idx[i] = kept;
                kept += 1;
            }
        }
        // Compact `verts` to those used by kept polys; remap vertex indices.
        let mut new_vert_idx = vec![u32::MAX; self.verts.len()];
        let mut verts = Vec::new();
        // Detail arrays: keep each kept poly's contiguous tri slice, compacting
        // `detail_verts` to the vertices those tris still reference.
        let mut new_dvert_idx = vec![u32::MAX; self.detail_verts.len()];
        let mut detail_verts = Vec::new();
        let mut detail_tris = Vec::new();
        let mut detail_meshes = Vec::new();
        let mut polys = Vec::with_capacity(kept as usize);
        for (i, poly) in self.polys.iter().enumerate() {
            if comp[i] != main {
                continue;
            }
            let mut p = poly.clone();
            for v in &mut p.verts {
                let nv = &mut new_vert_idx[*v as usize];
                if *nv == u32::MAX {
                    *nv = verts.len() as u32;
                    verts.push(self.verts[*v as usize]);
                }
                *v = *nv;
            }
            for nb in &mut p.neighbors {
                *nb = usize::try_from(*nb).map_or(-1, |n| new_poly_idx[n]);
            }
            if let Some(&[first, count]) = self.detail_meshes.get(i) {
                detail_meshes.push([detail_tris.len() as u32, count]);
                for t in &self.detail_tris[first as usize..(first + count) as usize] {
                    detail_tris.push(t.map(|dv| {
                        let ndv = &mut new_dvert_idx[dv as usize];
                        if *ndv == u32::MAX {
                            *ndv = detail_verts.len() as u32;
                            detail_verts.push(self.detail_verts[dv as usize]);
                        }
                        *ndv
                    }));
                }
            }
            polys.push(p);
        }
        let dropped = self.polys.len() - polys.len();
        let dropped_comps = comp_area.len() - 1;
        self.polys = polys;
        self.verts = verts;
        self.detail_verts = detail_verts;
        self.detail_tris = detail_tris;
        self.detail_meshes = detail_meshes;
        (self.polys.len(), dropped, dropped_comps)
    }
}

#[cfg(recast_cpp)]
const NULL_IDX: i32 = 0xffff;

/// Build a polygon navmesh from a world-space triangle soup. `obstacle` (if not
/// empty, len == tris.len()) flags triangles that block but are never walkable
/// (building walls/roofs); terrain/surface triangles should be `false`.
/// Recast area id stamped on drivable MODEL-surface polys (bridge/ramp decks), so
/// the runtime/viewer can tell them apart from terrain (`RC_WALKABLE_AREA` = 63).
pub const AREA_MODEL_SURFACE: u8 = 1;

/// A convex no-go prism: an XZ footprint polygon plus a vertical `[min_y, max_y]`
/// span, stamped `NOT_WALKABLE` on the compact heightfield (canonical Recast
/// `rcMarkConvexPolyArea`). Used to make a solid obstacle (rock/cliff) fully
/// non-walkable — including any terrain span baked under its footprint — so the
/// navmesh routes AROUND it and never ramps onto its top. Footprints must be
/// precise (tight per-piece convex hulls) so roads between obstacles survive.
#[derive(Clone, Debug)]
pub struct NoGoVolume {
    pub xz: Vec<[f32; 2]>,
    pub min_y: f32,
    pub max_y: f32,
}

/// Pure-Rust callers use [`build_rerecast`]; this C++ entry point is only compiled when the
/// recastnavigation source is vendored (see build.rs). Without it, `FUFLO_BACKEND=cpp` hits the
/// `#[cfg(not(recast_cpp))]` fallback below and returns an error (the default rerecast backend is used).
#[cfg(not(recast_cpp))]
pub fn build(
    _verts: &[[f32; 3]],
    _tris: &[[u32; 3]],
    _tri_source: &[u8],
    _nogo: &[NoGoVolume],
    _cfg: &BuildConfig,
) -> Result<PolyNavmesh, String> {
    Err("C++ Recast backend not compiled (recastnavigation not vendored under navmesh-recast/vendor) — \
         the default pure-Rust rerecast backend is used instead; unset FUFLO_BACKEND=cpp"
        .into())
}

/// `tri_source[i]` per-triangle role: 0 = terrain walkable-by-slope, 1 = obstacle
/// (forced `RC_NULL_AREA`, blocks), 2 = drivable model surface (walkable, stamped
/// `AREA_MODEL_SURFACE`). Empty slice => every triangle is plain walkable.
#[cfg(recast_cpp)]
pub fn build(
    verts: &[[f32; 3]],
    tris: &[[u32; 3]],
    tri_source: &[u8],
    // The C++ wrapper has no area-volume entry point; no-go volumes are honored only
    // by the default pure-Rust `build_rerecast`. Solid obstacles still block via their
    // collision triangles here, just without the rock-top terrain seal.
    _nogo: &[NoGoVolume],
    cfg: &BuildConfig,
) -> Result<PolyNavmesh, String> {
    if verts.is_empty() || tris.is_empty() {
        return Err("empty input geometry".into());
    }
    let vflat: Vec<c_float> = verts.iter().flat_map(|v| [v[0], v[1], v[2]]).collect();
    let tflat: Vec<c_int> = tris
        .iter()
        .flat_map(|t| [t[0] as c_int, t[1] as c_int, t[2] as c_int])
        .collect();
    let obs: Vec<c_uchar> = if tri_source.len() == tris.len() {
        tri_source.iter().map(|&s| s as c_uchar).collect()
    } else {
        Vec::new()
    };
    let obs_ptr = if obs.is_empty() {
        std::ptr::null()
    } else {
        obs.as_ptr()
    };

    let c = RcConfigC {
        cell_size: cfg.cell_size,
        cell_height: cfg.cell_height,
        walkable_slope_angle: cfg.walkable_slope_angle,
        agent_height: cfg.agent_height,
        agent_radius: cfg.agent_radius,
        agent_max_climb: cfg.agent_max_climb,
        edge_max_len: cfg.edge_max_len,
        edge_max_error: cfg.edge_max_error,
        region_min_size: cfg.region_min_size,
        region_merge_size: cfg.region_merge_size,
        detail_sample_dist: cfg.detail_sample_dist,
        detail_sample_max_error: cfg.detail_sample_max_error,
    };

    // SAFETY: the input pointers are derived from live local Vecs whose element counts are passed
    // alongside them, and `&c` outlives the call. On success `recast_build` returns a non-null
    // result (null-checked) owning buffers whose lengths are the `n*` fields we read back; each
    // from_raw_parts below uses exactly that length and is null-guarded. `recast_free(r)` releases
    // the result on every exit path before we return.
    unsafe {
        let r = recast_build(
            vflat.as_ptr(),
            verts.len() as c_int,
            tflat.as_ptr(),
            tris.len() as c_int,
            obs_ptr,
            &c,
        );
        if r.is_null() {
            return Err("recast_build returned null".into());
        }
        let res = &*r;
        if res.ok == 0 {
            let err = cstr(&res.err);
            recast_free(r);
            return Err(format!("recast build failed: {}", err));
        }

        let nverts = res.nverts.max(0) as usize;
        let nvp = res.nvp.max(0) as usize;
        let npolys = res.npolys.max(0) as usize;

        let mut out_verts = Vec::with_capacity(nverts);
        if nverts > 0 {
            let vs = std::slice::from_raw_parts(res.verts, nverts * 3);
            for i in 0..nverts {
                out_verts.push([vs[i * 3], vs[i * 3 + 1], vs[i * 3 + 2]]);
            }
        }

        let mut polys = Vec::with_capacity(npolys);
        if npolys > 0 && nvp > 0 {
            let ps = std::slice::from_raw_parts(res.polys, npolys * nvp * 2);
            let areas = std::slice::from_raw_parts(res.areas, npolys);
            for p in 0..npolys {
                let base = p * nvp * 2;
                let mut pv = Vec::new();
                for j in 0..nvp {
                    let vi = ps[base + j];
                    if vi != NULL_IDX {
                        pv.push(vi as u32);
                    }
                }
                let mut nb = Vec::new();
                for j in 0..nvp {
                    let n = ps[base + nvp + j];
                    nb.push(if n == NULL_IDX { -1 } else { n });
                }
                polys.push(Poly {
                    verts: pv,
                    neighbors: nb,
                    area: areas[p],
                });
            }
        }

        // Detail mesh (1-unit terrain-following triangulation).
        let ndverts = res.ndverts.max(0) as usize;
        let ndtris = res.ndtris.max(0) as usize;
        let mut detail_verts = Vec::with_capacity(ndverts);
        if ndverts > 0 && !res.dverts.is_null() {
            let ds = std::slice::from_raw_parts(res.dverts, ndverts * 3);
            for i in 0..ndverts {
                detail_verts.push([ds[i * 3], ds[i * 3 + 1], ds[i * 3 + 2]]);
            }
        }
        let mut detail_tris = Vec::with_capacity(ndtris);
        if ndtris > 0 && !res.dtris.is_null() {
            let dt = std::slice::from_raw_parts(res.dtris, ndtris * 3);
            for i in 0..ndtris {
                detail_tris.push([dt[i * 3] as u32, dt[i * 3 + 1] as u32, dt[i * 3 + 2] as u32]);
            }
        }
        let ndmeshes = res.ndmeshes.max(0) as usize;
        let mut detail_meshes = Vec::with_capacity(ndmeshes);
        if ndmeshes > 0 && !res.dmeshes.is_null() {
            let dm = std::slice::from_raw_parts(res.dmeshes, ndmeshes * 2);
            for i in 0..ndmeshes {
                detail_meshes.push([dm[i * 2] as u32, dm[i * 2 + 1] as u32]);
            }
        }

        recast_free(r);
        Ok(PolyNavmesh {
            verts: out_verts,
            nvp,
            polys,
            detail_verts,
            detail_tris,
            detail_meshes,
        })
    }
}

/// # Safety
/// `buf` must be a fully initialized 128-element C string buffer (as written by the FFI side).
#[cfg(recast_cpp)]
unsafe fn cstr(buf: &[c_char; 128]) -> String {
    // SAFETY: c_char and u8 share layout; we read the same 128 initialized elements as bytes.
    let bytes: &[u8] = std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len());
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod retain_tests {
    use super::*;

    fn square(vbase: u32, nb: [i32; 4]) -> Poly {
        Poly {
            verts: vec![vbase, vbase + 1, vbase + 2, vbase + 3],
            neighbors: nb.to_vec(),
            area: 63,
        }
    }

    /// Two components: polys 0+1 (adjacent 1x1 squares, total 2 m²) and poly 2
    /// (isolated 4 m² square). Largest-by-AREA keeps the single big poly even
    /// though the other component has more polys.
    #[test]
    fn keeps_largest_area_component_and_remaps() {
        let mut nav = PolyNavmesh {
            verts: vec![
                // component A: two unit squares sharing verts 1,2
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [2.0, 0.0, 0.0],
                [2.0, 0.0, 1.0],
                // component B: one 2x2 square far away
                [10.0, 0.0, 10.0],
                [12.0, 0.0, 10.0],
                [12.0, 0.0, 12.0],
                [10.0, 0.0, 12.0],
            ],
            nvp: 4,
            polys: vec![
                Poly {
                    verts: vec![0, 1, 2, 3],
                    neighbors: vec![-1, 1, -1, -1],
                    area: 63,
                },
                Poly {
                    verts: vec![1, 4, 5, 2],
                    neighbors: vec![-1, -1, -1, 0],
                    area: 63,
                },
                square(6, [-1, -1, -1, -1]),
            ],
            detail_verts: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [10.0, 0.0, 10.0],
                [12.0, 0.0, 10.0],
                [12.0, 0.0, 12.0],
            ],
            detail_tris: vec![[0, 1, 2], [0, 1, 2], [3, 4, 5]],
            detail_meshes: vec![[0, 1], [1, 1], [2, 1]],
        };
        let (kept, dropped, comps) = nav.retain_largest_component();
        assert_eq!((kept, dropped, comps), (1, 2, 1));
        assert_eq!(nav.polys.len(), 1);
        assert_eq!(nav.verts.len(), 4, "verts compacted to the kept square");
        assert_eq!(
            nav.polys[0].neighbors,
            vec![-1, -1, -1, -1],
            "no stale poly indices"
        );
        assert_eq!(nav.verts[nav.polys[0].verts[0] as usize], [10.0, 0.0, 10.0]);
        assert_eq!(nav.detail_meshes, vec![[0, 1]]);
        assert_eq!(nav.detail_tris, vec![[0, 1, 2]], "detail verts remapped");
        assert_eq!(nav.detail_verts[0], [10.0, 0.0, 10.0]);
    }

    /// A single connected mesh is untouched.
    #[test]
    fn single_component_untouched() {
        let mut nav = PolyNavmesh {
            verts: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
            nvp: 4,
            polys: vec![square(0, [-1, -1, -1, -1])],
            detail_verts: vec![],
            detail_tris: vec![],
            detail_meshes: vec![[0, 0]],
        };
        let before = nav.clone();
        let (kept, dropped, comps) = nav.retain_largest_component();
        assert_eq!((kept, dropped, comps), (1, 0, 0));
        assert_eq!(nav.polys.len(), before.polys.len());
        assert_eq!(nav.verts, before.verts);
    }
}
