//! Pure-Rust Recast backend via the `rerecast` crate — the PERMANENT, default
//! navmesh build path. It runs the Recast solo-mesh pipeline and returns the
//! [`PolyNavmesh`] (convex polys + neighbours + areas + detail mesh + per-poly
//! detail ranges) every downstream consumer (`recast_to_nav3d`, the runtime, the
//! viewer) already consumes, so no consumer is backend-aware.
//!
//! Production wiring (see `navmesh-gen` `generate_recast`): this backend, driven
//! tile-by-tile through [`crate::build_rerecast_tiled`], is what ships. The
//! `FUFLO_*` environment variables are OVERRIDES / escape hatches, not experiments:
//!   * `FUFLO_BACKEND=cpp` falls back to the hand-rolled C++ Recast wrapper.
//!   * `FUFLO_NOTILE=1` forces the single-tile [`build_rerecast`] path here.
//! Absent those, the default is always pure-Rust rerecast, multi-tiled.
//!
//! Config quantisation mirrors `wrapper/recast_wrapper.cpp` 1:1 (cs/ch, ceil/round
//! on height/climb/radius, region areas) so the two backends produce comparable
//! navmeshes. The one intentional difference: `border_size = walkable_radius + 3`
//! (the rerecast canonical value) vs the C++ wrapper's implicit 0 — a small map-edge
//! coverage difference, irrelevant to interior passability.

use glam::{UVec3, Vec2, Vec3A};
use rerecast::{
    Aabb3d, AreaType, BuildContoursFlags, Config, ConvexVolume, DetailNavmesh, HeightfieldBuilder,
    PolygonNavmesh, TriMesh,
};

use crate::{BuildConfig, NoGoVolume, Poly, PolyNavmesh};

/// Build a [`PolyNavmesh`] from a world-space triangle soup using the pure-Rust
/// `rerecast` pipeline. `tri_source[i]`: 0 = terrain (walkable by slope), 1 =
/// obstacle (rasterised as a non-walkable blocker span), 2 = drivable model
/// surface / deck (walkable, area id 1). Empty `tri_source` => all terrain.
pub fn build_rerecast(
    verts: &[[f32; 3]],
    tris: &[[u32; 3]],
    tri_source: &[u8],
    nogo: &[NoGoVolume],
    cfg: &BuildConfig,
) -> Result<PolyNavmesh, String> {
    Ok(build_one(verts, tris, tri_source, nogo, cfg, None)?.0)
}

/// Per-poly, per-edge portal direction emitted by rerecast's tiling path
/// (`0x8000 | dir`, dir 0=west/x.min, 1=north/z.max, 2=east/x.max, 3=south/z.min),
/// or `-1` for a non-portal edge. Aligned 1:1 with [`PolyNavmesh::polys`] and each
/// poly's edge slots. Only meaningful when `border_size > 0` (always true here).
pub(crate) type PortalDirs = Vec<Vec<i8>>;

/// Build a single [`PolyNavmesh`] from a triangle soup. When `aabb_override` is
/// `Some`, it replaces the mesh-derived AABB for both the [`Config`] and the
/// heightfield, so a tiled build can force every tile onto one shared, grid-aligned
/// voxel lattice (identical origin + cell size) and weld border verts losslessly.
pub(crate) fn build_one(
    verts: &[[f32; 3]],
    tris: &[[u32; 3]],
    tri_source: &[u8],
    nogo: &[NoGoVolume],
    cfg: &BuildConfig,
    aabb_override: Option<Aabb3d>,
) -> Result<(PolyNavmesh, PortalDirs), String> {
    if verts.is_empty() || tris.is_empty() {
        return Err("empty input geometry".into());
    }
    let ntris = tris.len();
    let slope_rad = cfg.walkable_slope_angle.to_radians();

    // --- input mesh + per-triangle area roles ---
    let mut mesh = TriMesh {
        vertices: verts.iter().map(|v| Vec3A::from_array(*v)).collect(),
        indices: tris.iter().map(|t| UVec3::from_array(*t)).collect(),
        area_types: vec![AreaType(0); ntris],
    };
    // Let rerecast's own slope test decide walkability (flat -> 255, steep -> 0),
    // then remap to our role-specific area ids. Mirrors the C++ wrapper's
    // rcMarkWalkableTriangles + per-triangle override.
    mesh.mark_walkable_triangles(slope_rad);
    // Winding-blind up test for DECK tris: the deck feed accepts |up| >= cos(slope)
    // because bridge Havok meshes wind their drive-on top inconsistently
    // (105_germany HugoBridge's roadway is wound face-DOWN; the tank still drives
    // on it — the engine resolves onto whichever face is on top). The canonical
    // SIGNED slope test above nulls those tris, silently deleting the whole
    // mid-span, so re-test src==2 with the absolute normal.
    let cos_slope = slope_rad.cos();
    let abs_up = |t: &UVec3| -> f32 {
        let a = mesh.vertices[t.x as usize];
        let b = mesh.vertices[t.y as usize];
        let c = mesh.vertices[t.z as usize];
        let n = (b - a).cross(c - a);
        let len = n.length();
        if len > 1e-9 {
            (n.y / len).abs()
        } else {
            0.0
        }
    };
    for i in 0..ntris {
        let walkable = mesh.area_types[i].0 != 0;
        let src = tri_source.get(i).copied().unwrap_or(0);
        mesh.area_types[i] = AreaType(match src {
            1 => 0, // obstacle: blocker span, never walkable
            // deck = AREA_MODEL_SURFACE (1), winding-blind
            2 => {
                if walkable || abs_up(&mesh.indices[i]) >= cos_slope {
                    1
                } else {
                    0
                }
            }
            _ => {
                if walkable {
                    63
                } else {
                    0
                }
            } // terrain = RC_WALKABLE_AREA (63)
        });
    }

    // --- config (quantised exactly like the C++ wrapper) ---
    let aabb = match aabb_override {
        Some(a) => a,
        None => mesh.compute_aabb().ok_or("empty mesh aabb")?,
    };
    let cs = cfg.cell_size;
    let ch = cfg.cell_height;
    let walkable_radius = (cfg.agent_radius / cs).ceil() as u16;
    let config = Config {
        width: (((aabb.max.x - aabb.min.x) / cs) + 0.5) as u16,
        height: (((aabb.max.z - aabb.min.z) / cs) + 0.5) as u16,
        tile_size: 0,
        border_size: walkable_radius + 3,
        cell_size: cs,
        cell_height: ch,
        aabb,
        walkable_slope_angle: slope_rad,
        walkable_height: (cfg.agent_height / ch).ceil() as u16,
        // FLOOR the climb (canonical Recast: walkableClimb = floor(agentMaxClimb/ch),
        // RecastDemo Sample_SoloMesh.cpp). ROUND let a cliff staircase whose per-cell
        // rise was < climb*ch read as climbable so the ledge filter never severed the
        // rim; floor tightens it (1.0/0.4 -> 2 = 0.8m instead of 3 = 1.2m), so a steeper
        // rock face exceeds walkable_climb and rcFilterLedgeSpans nulls its rim.
        walkable_climb: (cfg.agent_max_climb / ch).floor() as u16,
        walkable_radius,
        max_edge_len: (cfg.edge_max_len / cs) as u16,
        max_simplification_error: cfg.edge_max_error,
        min_region_area: (cfg.region_min_size * cfg.region_min_size) as u16,
        merge_region_area: (cfg.region_merge_size * cfg.region_merge_size) as u16,
        max_vertices_per_polygon: 6,
        detail_sample_dist: if cfg.detail_sample_dist < 0.9 {
            0.0
        } else {
            cs * cfg.detail_sample_dist
        },
        detail_sample_max_error: ch * cfg.detail_sample_max_error,
        contour_flags: BuildContoursFlags::default(),
        area_volumes: Vec::new(),
    };

    // --- pipeline (canonical rerecast solo-mesh order) ---
    let mut hf = HeightfieldBuilder {
        aabb: config.aabb,
        cell_size: config.cell_size,
        cell_height: config.cell_height,
    }
    .build()
    .map_err(|e| format!("heightfield: {e}"))?;
    hf.rasterize_triangles(&mesh, config.walkable_climb)
        .map_err(|e| format!("rasterize: {e}"))?;
    // Per-area step for the drivable deck (area id 1). A bridge/viaduct deck
    // rasterises ~1-2 m above the terrain it abuts; the terrain `walkable_climb`
    // is kept tight (~0.8 m) so a tank cannot climb a rock rim, which would also
    // sever the deck from the ground at its ramps and isolate it (the pathfinder
    // then routes around the bridge). `deck_climb` grants ONLY deck spans (and the
    // ground spans abutting them) a ~2 m step so the deck stitches to ground while
    // terrain/rock rims keep the tight climb. Quantised the same way as the global
    // climb. FUFLO_DECK_CLIMB overrides (metres).
    let deck_climb = {
        let m = std::env::var("FUFLO_DECK_CLIMB")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(2.0);
        ((m / ch).round() as u16).max(config.walkable_climb)
    };
    hf.filter_low_hanging_walkable_obstacles(config.walkable_climb, deck_climb);
    hf.filter_ledge_spans(config.walkable_height, config.walkable_climb, deck_climb);
    hf.filter_walkable_low_height_spans(config.walkable_height);

    let mut chf = hf
        .into_compact(config.walkable_height, config.walkable_climb, deck_climb)
        .map_err(|e| format!("compact heightfield: {e}"))?;
    if config.walkable_radius > 0 {
        chf.erode_walkable_area(config.walkable_radius);
    }
    // Stamp no-go prisms NOT_WALKABLE (canonical rcMarkConvexPolyArea order: after
    // erosion, before regions). Nulls every span in the footprint — including terrain
    // baked under a rock/cliff — so the navmesh routes around it, never onto its top.
    for v in nogo {
        if v.xz.len() < 3 {
            continue;
        }
        let volume = ConvexVolume {
            vertices: v.xz.iter().map(|p| Vec2::new(p[0], p[1])).collect(),
            min_y: v.min_y,
            max_y: v.max_y,
            area: AreaType::NOT_WALKABLE,
        };
        chf.mark_convex_poly_area(&volume);
    }
    chf.build_distance_field();
    chf.build_regions(
        config.border_size,
        config.min_region_area,
        config.merge_region_area,
    )
    .map_err(|e| format!("regions: {e}"))?;

    let contours = chf.build_contours(
        config.max_simplification_error,
        config.max_edge_len,
        config.contour_flags,
    );
    let poly = contours
        .into_polygon_mesh(config.max_vertices_per_polygon)
        .map_err(|e| format!("poly mesh: {e}"))?;
    let dmesh = DetailNavmesh::new(
        &poly,
        &chf,
        config.detail_sample_dist,
        config.detail_sample_max_error,
    )
    .map_err(|e| format!("detail mesh: {e:?}"))?;

    Ok(convert(&poly, &dmesh))
}

/// Map rerecast's split-Vec [`PolygonNavmesh`] + [`DetailNavmesh`] into our
/// [`PolyNavmesh`] (same shape the C++ wrapper produces), plus the per-edge portal
/// directions ([`PortalDirs`]) so a tiled merge can re-link cross-tile seams.
fn convert(poly: &PolygonNavmesh, dmesh: &DetailNavmesh) -> (PolyNavmesh, PortalDirs) {
    let nvp = poly.max_vertices_per_polygon as usize;

    // Convex poly verts: voxel (U16Vec3) -> world.
    let verts: Vec<[f32; 3]> = poly
        .vertices
        .iter()
        .map(|v| {
            [
                poly.aabb.min.x + v.x as f32 * poly.cell_size,
                poly.aabb.min.y + v.y as f32 * poly.cell_height,
                poly.aabb.min.z + v.z as f32 * poly.cell_size,
            ]
        })
        .collect();

    // Near-zero-area degenerate polys are contour-simplification needles (the
    // simplifier collapses a thin walkable strip's two near-parallel boundaries into
    // a sliver). They carry no traversable surface (a tank is ~3 m wide) yet render as
    // visual diagonal spikes. Drop any convex poly whose XZ area is below this floor
    // to an empty PLACEHOLDER, which keeps every poly index (and the 1:1 detail-mesh
    // alignment) valid while excluding it from A*/locate (PolyNavmesh3d skips <3-vert
    // polys). Keep the threshold well below tank width so no real corridor is removed.
    // FUFLO_DEGEN_AREA overrides (m^2); 0 disables.
    let degen_area: f32 = std::env::var("FUFLO_DEGEN_AREA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.25);
    let poly_xz_area = |pv: &[u32]| -> f32 {
        if pv.len() < 3 {
            return 0.0;
        }
        let mut a2 = 0.0f32;
        let p0 = verts[pv[0] as usize];
        for w in pv[1..].windows(2) {
            let p1 = verts[w[0] as usize];
            let p2 = verts[w[1] as usize];
            a2 += (p1[0] - p0[0]) * (p2[2] - p0[2]) - (p2[0] - p0[0]) * (p1[2] - p0[2]);
        }
        a2.abs() * 0.5
    };

    let npolys = poly.polygon_count();
    let mut polys = Vec::with_capacity(npolys);
    let mut portals: PortalDirs = Vec::with_capacity(npolys);
    for (p, area) in poly.areas.iter().enumerate().take(npolys) {
        let base = p * nvp;
        let mut pv = Vec::new();
        for j in 0..nvp {
            let vi = poly.polygons[base + j];
            if vi == PolygonNavmesh::NO_INDEX {
                break;
            }
            pv.push(vi as u32);
        }
        if degen_area > 0.0 && poly_xz_area(&pv) < degen_area {
            // Placeholder: empty verts/neighbours, index preserved (an empty `verts`
            // marks it dropped for the sever pass below).
            polys.push(Poly {
                verts: Vec::new(),
                neighbors: Vec::new(),
                area: 0,
            });
            portals.push(Vec::new());
            continue;
        }
        let mut nb = Vec::with_capacity(pv.len());
        let mut pd = Vec::with_capacity(pv.len());
        for j in 0..pv.len() {
            let n = poly.polygon_neighbors[base + j];
            // NO_CONNECTION (0xffff) = solid border; the 0x8000 bit tags
            // border-region portal edges. A solo build wants both treated as "no
            // neighbour" (A* never crosses a non-edge); a tiled build records the
            // portal DIRECTION (low 2 bits) so the seam merge can re-link it.
            if n == PolygonNavmesh::NO_CONNECTION {
                nb.push(-1);
                pd.push(-1);
            } else if (n & 0x8000) != 0 {
                nb.push(-1);
                pd.push((n & 0x3) as i8);
            } else {
                nb.push(n as i32);
                pd.push(-1);
            }
        }
        polys.push(Poly {
            verts: pv,
            neighbors: nb,
            area: area.0,
        });
        portals.push(pd);
    }
    // Sever links pointing at a dropped placeholder so A*/component-walk never
    // expands into an empty poly (a sub-tank sliver carried no real connection).
    let dropped: Vec<bool> = polys.iter().map(|p| p.verts.is_empty()).collect();
    for poly in polys.iter_mut() {
        for nb in poly.neighbors.iter_mut() {
            if *nb >= 0 && dropped[*nb as usize] {
                *nb = -1;
            }
        }
    }

    // Detail mesh: per-submesh local indices -> flat global-indexed tris, plus the
    // per-poly tri range (aligned 1:1 with polys, since meshes are in poly order).
    let detail_verts: Vec<[f32; 3]> = dmesh.vertices.iter().map(|v| [v.x, v.y, v.z]).collect();
    let mut detail_tris: Vec<[u32; 3]> = Vec::with_capacity(dmesh.triangles.len());
    let mut detail_meshes: Vec<[u32; 2]> = Vec::with_capacity(dmesh.meshes.len());
    for sm in &dmesh.meshes {
        let first = detail_tris.len() as u32;
        let bvert = sm.base_vertex_index;
        let bt = sm.base_triangle_index as usize;
        let ct = sm.triangle_count as usize;
        for t in &dmesh.triangles[bt..bt + ct] {
            detail_tris.push([
                bvert + t[0] as u32,
                bvert + t[1] as u32,
                bvert + t[2] as u32,
            ]);
        }
        detail_meshes.push([first, ct as u32]);
    }

    (
        PolyNavmesh {
            verts,
            nvp,
            polys,
            detail_verts,
            detail_tris,
            detail_meshes,
        },
        portals,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Triangulate an axis-aligned horizontal slab top at `y` spanning
    /// [x0,x1]x[z0,z1] into ~2m quads (two tris each), all tagged `src`.
    fn slab(
        verts: &mut Vec<[f32; 3]>,
        tris: &mut Vec<[u32; 3]>,
        src_v: &mut Vec<u8>,
        (x0, x1, z0, z1, y): (f32, f32, f32, f32, f32),
        src: u8,
    ) {
        let step = 2.0f32;
        let nx = ((x1 - x0) / step).ceil() as usize;
        let nz = ((z1 - z0) / step).ceil() as usize;
        for iz in 0..nz {
            for ix in 0..nx {
                let ax = x0 + ix as f32 * step;
                let az = z0 + iz as f32 * step;
                let bx = (ax + step).min(x1);
                let bz = (az + step).min(z1);
                let base = verts.len() as u32;
                verts.push([ax, y, az]);
                verts.push([bx, y, az]);
                verts.push([bx, y, bz]);
                verts.push([ax, y, bz]);
                // wind CCW seen from +Y (up-facing normal) or the slope test
                // rejects the slab outright
                tris.push([base, base + 2, base + 1]);
                tris.push([base, base + 3, base + 2]);
                src_v.push(src);
                src_v.push(src);
            }
        }
    }

    fn production_cfg() -> BuildConfig {
        BuildConfig {
            agent_radius: 2.0,
            cell_size: 0.23,
            cell_height: 0.2,
            detail_sample_dist: 6.0,
            ..BuildConfig::default()
        }
    }

    /// Regression: a bridge deck (area src=2) FLOATING over a carved void (deep
    /// water removed all terrain beneath — 105_germany HugoBridge) must still
    /// produce navmesh polys. Guards the raster/filter/region pipeline against
    /// dropping walkable spans whose columns have no other geometry.
    #[test]
    fn floating_deck_over_void_produces_polys() {
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        let mut src = Vec::new();
        // 60 x 18 m roadway at y = 0.5, nothing else in the world.
        slab(
            &mut verts,
            &mut tris,
            &mut src,
            (0.0, 60.0, 0.0, 18.0, 0.5),
            2,
        );
        let cfg = production_cfg();
        let (nav, _portals) =
            build_one(&verts, &tris, &src, &[], &cfg, None).expect("build_one failed");
        assert!(
            !nav.polys.is_empty(),
            "floating deck produced no polys (HugoBridge regression)"
        );
    }

    /// A deck wound face-DOWN (inconsistent bridge Havok winding — 105_germany
    /// HugoBridge roadway) must still produce polys: the deck feed is
    /// winding-blind (`|up| >= cos`), so the backend must be too.
    #[test]
    fn down_wound_floating_deck_produces_polys() {
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        let mut src = Vec::new();
        slab(
            &mut verts,
            &mut tris,
            &mut src,
            (0.0, 60.0, 0.0, 18.0, 0.5),
            2,
        );
        // invert winding of every tri -> normals face down
        for t in &mut tris {
            t.swap(1, 2);
        }
        let cfg = production_cfg();
        let (nav, _portals) =
            build_one(&verts, &tris, &src, &[], &cfg, None).expect("build_one failed");
        assert!(
            !nav.polys.is_empty(),
            "down-wound floating deck produced no polys (HugoBridge regression)"
        );
    }

    /// Same deck with a riverbed plane 10 m below (the pre-water-carve situation,
    /// analogous to Redshire StoneBridge over a ravine floor): must also produce
    /// deck polys. If this passes while `floating_deck_over_void_produces_polys`
    /// fails, the pipeline mishandles columns with a single floating span.
    #[test]
    fn deck_over_riverbed_produces_polys() {
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        let mut src = Vec::new();
        slab(
            &mut verts,
            &mut tris,
            &mut src,
            (0.0, 60.0, 0.0, 18.0, 0.5),
            2,
        );
        slab(
            &mut verts,
            &mut tris,
            &mut src,
            (-10.0, 70.0, -10.0, 28.0, -10.0),
            0,
        );
        let cfg = production_cfg();
        let (nav, _portals) =
            build_one(&verts, &tris, &src, &[], &cfg, None).expect("build_one failed");
        let decks = nav.polys.iter().filter(|p| p.area == 1).count();
        assert!(
            decks > 0,
            "deck over riverbed produced no area-1 polys ({} total)",
            nav.polys.len()
        );
    }
}
