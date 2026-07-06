//! Milestone-0 tiled build: rasterise the world tile-by-tile (each tile forced
//! onto one shared, global grid-aligned voxel lattice), then MERGE the per-tile
//! convex-poly meshes into ONE flat [`PolyNavmesh`] by welding coincident border
//! verts and rebuilding the entire neighbour adjacency from global undirected-edge
//! matching. The runtime/pathfinder traverse only the flat `neighbors` array, so
//! correct cross-tile connectivity is purely a function of that rebuilt adjacency.
//!
//! At the same `cell_size`, this is connectivity-equivalent to the single-tile
//! [`build_rerecast`](crate::build_rerecast): tiles share the lattice (border verts
//! coincide), each tile keeps only polys whose XZ centroid falls in its core, and
//! the global edge match re-stitches the seams.

use glam::Vec3;
use rerecast::Aabb3d;

use crate::rerecast_backend::{build_one, PortalDirs};
use crate::{BuildConfig, NoGoVolume, Poly, PolyNavmesh};

/// Build a flat [`PolyNavmesh`] by tiling the world at `tile_size_m` (snapped to a
/// whole number of cells), building each tile on a shared grid-aligned lattice, and
/// merging the kept core polys with welded borders + rebuilt adjacency.
pub fn build_rerecast_tiled(
    verts: &[[f32; 3]],
    tris: &[[u32; 3]],
    tri_source: &[u8],
    nogo: &[NoGoVolume],
    cfg: &BuildConfig,
    tile_size_m: f32,
) -> Result<PolyNavmesh, String> {
    // Seam-merge mode. Default `inject` = T-junction resolution: weld on-seam verts,
    // inject every seam breakpoint into the opposite tile's portal edge so the exact
    // undirected edge-match links each sub-segment with no heuristic. `FUFLO_TILE_MODE`
    // A/B knobs: `portal` = the pre-injection native portal-overlap linker; `weld` = the
    // legacy geometric lattice-line overlap heuristic.
    let tile_mode = std::env::var("FUFLO_TILE_MODE").unwrap_or_default();
    let merge_mode = if tile_mode.eq_ignore_ascii_case("weld") {
        MergeMode::Weld
    } else if tile_mode.eq_ignore_ascii_case("portal") {
        MergeMode::Portal
    } else {
        MergeMode::Inject
    };
    build_tiled(verts, tris, tri_source, nogo, cfg, tile_size_m, merge_mode)
}

/// [`build_rerecast_tiled`] with the seam-merge mode passed explicitly (testable
/// without the `FUFLO_TILE_MODE` env knob).
fn build_tiled(
    verts: &[[f32; 3]],
    tris: &[[u32; 3]],
    tri_source: &[u8],
    nogo: &[NoGoVolume],
    cfg: &BuildConfig,
    tile_size_m: f32,
    merge_mode: MergeMode,
) -> Result<PolyNavmesh, String> {
    if verts.is_empty() || tris.is_empty() {
        return Err("empty input geometry".into());
    }
    let cs = cfg.cell_size;

    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for v in verts {
        for k in 0..3 {
            min[k] = min[k].min(v[k]);
            max[k] = max[k].max(v[k]);
        }
    }
    let origin = [min[0], min[2]];
    let world_max = [max[0], max[2]];

    let tile_w = (tile_size_m / cs).round().max(8.0) * cs;
    // build_one removes the `border_size = walkable_radius + 3` cell-wide rim during
    // region building, so polys terminate exactly at the core boundary (a shared,
    // grid-aligned seam line) when the expanded margin equals that border. Adjacent
    // tiles then emit coincident seam verts that weld + edge-match cleanly.
    let walkable_radius = (cfg.agent_radius / cs).ceil();
    let border_margin = (walkable_radius + 3.0) * cs;

    let snap_down = |x: f32, o: f32| o + ((x - o) / cs).floor() * cs;
    let snap_up = |x: f32, o: f32| o + ((x - o) / cs).ceil() * cs;

    let nx = (((world_max[0] - origin[0]) / tile_w).ceil() as usize).max(1);
    let nz = (((world_max[1] - origin[1]) / tile_w).ceil() as usize).max(1);

    let mut merged: Vec<KeptPoly> = Vec::new();

    for iz in 0..nz {
        for ix in 0..nx {
            let core_min = [
                origin[0] + ix as f32 * tile_w,
                origin[1] + iz as f32 * tile_w,
            ];
            let core_max = [
                (core_min[0] + tile_w).min(world_max[0]),
                (core_min[1] + tile_w).min(world_max[1]),
            ];
            let exp_min = [
                snap_down(core_min[0] - border_margin, origin[0]),
                snap_down(core_min[1] - border_margin, origin[1]),
            ];
            let exp_max = [
                snap_up(core_max[0] + border_margin, origin[0]),
                snap_up(core_max[1] + border_margin, origin[1]),
            ];

            let (sub_verts, sub_tris, sub_src) =
                cull_mesh(verts, tris, tri_source, exp_min, exp_max);
            if sub_tris.is_empty() {
                continue;
            }
            let sub_nogo = cull_nogo(nogo, exp_min, exp_max);

            let tile_aabb = Aabb3d {
                min: Vec3::new(exp_min[0], min[1], exp_min[1]),
                max: Vec3::new(exp_max[0], max[1], exp_max[1]),
            };
            let (tile_nav, tile_portals) = build_one(
                &sub_verts,
                &sub_tris,
                &sub_src,
                &sub_nogo,
                cfg,
                Some(tile_aabb),
            )?;

            collect_core_polys(&tile_nav, &tile_portals, core_min, core_max, &mut merged);
        }
    }

    Ok(merge(
        merged,
        SeamLattice { origin, tile_w, cs },
        cfg.cell_height,
        MAX_VERTS_PER_POLY,
        merge_mode,
    ))
}

const MAX_VERTS_PER_POLY: usize = 6;

/// How cross-tile seams get re-linked in [`merge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MergeMode {
    /// T-junction resolution: weld on-seam verts, inject seam breakpoints into
    /// portal edges, then exact undirected edge-match (the default).
    Inject,
    /// Native portal-marker overlap linking (the pre-injection A/B path).
    Portal,
    /// Legacy geometric lattice-line overlap heuristic.
    Weld,
}

struct KeptPoly {
    verts: Vec<[f32; 3]>,
    area: u8,
    /// Per-edge portal direction (0..3) from rerecast's tiling path, or -1 for a
    /// non-portal edge. Aligned 1:1 with `verts` edge slots (edge j = verts[j]->[j+1]).
    portal_dirs: Vec<i8>,
    detail_verts: Vec<[f32; 3]>,
    detail_tris: Vec<[u32; 3]>,
}

fn cull_mesh(
    verts: &[[f32; 3]],
    tris: &[[u32; 3]],
    tri_source: &[u8],
    exp_min: [f32; 2],
    exp_max: [f32; 2],
) -> (Vec<[f32; 3]>, Vec<[u32; 3]>, Vec<u8>) {
    let mut remap = vec![u32::MAX; verts.len()];
    let mut sub_verts: Vec<[f32; 3]> = Vec::new();
    let mut sub_tris: Vec<[u32; 3]> = Vec::new();
    let mut sub_src: Vec<u8> = Vec::new();

    for (ti, t) in tris.iter().enumerate() {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        let tmin_x = a[0].min(b[0]).min(c[0]);
        let tmax_x = a[0].max(b[0]).max(c[0]);
        let tmin_z = a[2].min(b[2]).min(c[2]);
        let tmax_z = a[2].max(b[2]).max(c[2]);
        if tmax_x < exp_min[0] || tmin_x > exp_max[0] || tmax_z < exp_min[1] || tmin_z > exp_max[1]
        {
            continue;
        }
        let mut local = [0u32; 3];
        for (k, &gi) in t.iter().enumerate() {
            if remap[gi as usize] == u32::MAX {
                remap[gi as usize] = sub_verts.len() as u32;
                sub_verts.push(verts[gi as usize]);
            }
            local[k] = remap[gi as usize];
        }
        sub_tris.push(local);
        sub_src.push(tri_source.get(ti).copied().unwrap_or(0));
    }
    (sub_verts, sub_tris, sub_src)
}

fn cull_nogo(nogo: &[NoGoVolume], exp_min: [f32; 2], exp_max: [f32; 2]) -> Vec<NoGoVolume> {
    nogo.iter()
        .filter(|v| {
            if v.xz.is_empty() {
                return false;
            }
            let mut nmin = [f32::MAX; 2];
            let mut nmax = [f32::MIN; 2];
            for p in &v.xz {
                nmin[0] = nmin[0].min(p[0]);
                nmin[1] = nmin[1].min(p[1]);
                nmax[0] = nmax[0].max(p[0]);
                nmax[1] = nmax[1].max(p[1]);
            }
            !(nmax[0] < exp_min[0]
                || nmin[0] > exp_max[0]
                || nmax[1] < exp_min[1]
                || nmin[1] > exp_max[1])
        })
        .cloned()
        .collect()
}

fn collect_core_polys(
    tile_nav: &PolyNavmesh,
    tile_portals: &PortalDirs,
    core_min: [f32; 2],
    core_max: [f32; 2],
    out: &mut Vec<KeptPoly>,
) {
    for (pi, poly) in tile_nav.polys.iter().enumerate() {
        if poly.verts.len() < 3 {
            continue;
        }
        let mut cx = 0.0f32;
        let mut cz = 0.0f32;
        for &vi in &poly.verts {
            let w = tile_nav.verts[vi as usize];
            cx += w[0];
            cz += w[2];
        }
        let n = poly.verts.len() as f32;
        cx /= n;
        cz /= n;
        if cx < core_min[0] || cx >= core_max[0] || cz < core_min[1] || cz >= core_max[1] {
            continue;
        }

        let world: Vec<[f32; 3]> = poly
            .verts
            .iter()
            .map(|&vi| tile_nav.verts[vi as usize])
            .collect();

        let (dv, dt) = detail_slice(tile_nav, pi).unwrap_or_default();
        let portal_dirs = tile_portals
            .get(pi)
            .cloned()
            .unwrap_or_else(|| vec![-1; world.len()]);

        out.push(KeptPoly {
            verts: world,
            area: poly.area,
            portal_dirs,
            detail_verts: dv,
            detail_tris: dt,
        });
    }
}

type DetailSlice = (Vec<[f32; 3]>, Vec<[u32; 3]>);

/// The detail submesh for poly `pi` in `nav`, re-indexed to a local `detail_verts`
/// slice (only the verts this poly's tris reference, remapped to 0-based).
fn detail_slice(nav: &PolyNavmesh, pi: usize) -> Option<DetailSlice> {
    let [first, count] = *nav.detail_meshes.get(pi)?;
    let (first, count) = (first as usize, count as usize);
    let tris = nav.detail_tris.get(first..first + count)?;
    let mut remap: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut dv: Vec<[f32; 3]> = Vec::new();
    let mut dt: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
    for t in tris {
        let mut lt = [0u32; 3];
        for (k, &gi) in t.iter().enumerate() {
            let li = *remap.entry(gi).or_insert_with(|| {
                let idx = dv.len() as u32;
                dv.push(nav.detail_verts[gi as usize]);
                idx
            });
            lt[k] = li;
        }
        dt.push(lt);
    }
    Some((dv, dt))
}

fn merge(
    kept: Vec<KeptPoly>,
    lat: SeamLattice,
    ch: f32,
    nvp: usize,
    mode: MergeMode,
) -> PolyNavmesh {
    let kx = lat.cs * 0.1;
    let ky = ch * 0.5;
    let kz = lat.cs * 0.1;
    let key = |p: [f32; 3]| -> (i64, i64, i64) {
        (
            (p[0] / kx).round() as i64,
            (p[1] / ky).round() as i64,
            (p[2] / kz).round() as i64,
        )
    };

    let mut weld: std::collections::HashMap<(i64, i64, i64), u32> =
        std::collections::HashMap::new();
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut polys: Vec<Poly> = Vec::with_capacity(kept.len());
    let mut detail_verts: Vec<[f32; 3]> = Vec::new();
    let mut detail_tris: Vec<[u32; 3]> = Vec::new();
    let mut detail_meshes: Vec<[u32; 2]> = Vec::with_capacity(kept.len());
    let mut portal_dirs: Vec<Vec<i8>> = Vec::with_capacity(kept.len());

    for kp in &kept {
        let gverts: Vec<u32> = kp
            .verts
            .iter()
            .map(|&w| {
                *weld.entry(key(w)).or_insert_with(|| {
                    let idx = verts.len() as u32;
                    verts.push(w);
                    idx
                })
            })
            .collect();
        let m = gverts.len();
        polys.push(Poly {
            verts: gverts,
            neighbors: vec![-1; m],
            area: kp.area,
        });
        let mut pd = kp.portal_dirs.clone();
        pd.resize(m, -1);
        portal_dirs.push(pd);

        let dv_base = detail_verts.len() as u32;
        detail_verts.extend_from_slice(&kp.detail_verts);
        let first = detail_tris.len() as u32;
        for t in &kp.detail_tris {
            detail_tris.push([t[0] + dv_base, t[1] + dv_base, t[2] + dv_base]);
        }
        detail_meshes.push([first, kp.detail_tris.len() as u32]);
    }

    match mode {
        MergeMode::Inject => {
            inject_seam_breakpoints(&mut polys, &mut verts, &mut portal_dirs, lat, ch);
            rebuild_adjacency(&mut polys);
            // Injection makes every seam vertex exist on BOTH sides, so the exact edge
            // match links every seam edge that HAS an opposite portal. The rare leftover
            // is a portal edge whose opposite tile produced no portal there (per-tile
            // region asymmetry near the border, not a T-junction); the portal-overlap
            // linker recovers those so they don't leave a connectivity gap.
            link_portals(&mut polys, &verts, &portal_dirs, lat.cs);
        }
        MergeMode::Portal => {
            rebuild_adjacency(&mut polys);
            link_portals(&mut polys, &verts, &portal_dirs, lat.cs);
        }
        MergeMode::Weld => {
            rebuild_adjacency(&mut polys);
            link_seams(&mut polys, &verts, lat);
        }
    }

    PolyNavmesh {
        verts,
        nvp,
        polys,
        detail_verts,
        detail_tris,
        detail_meshes,
    }
}

/// Re-link border edges that lie on an interior tile-seam line but were left
/// unmatched by the exact-vertex edge rebuild (the two tiles simplified the seam
/// contour to non-coincident verts, or border-vert Y drifted past the weld tol).
/// Restricted to seam lines so it can never punch through a real interior wall:
/// an edge qualifies only if BOTH endpoints sit on the same `k*tile_w` lattice
/// line, and it is linked only to an overlapping edge whose owning poly's centroid
/// is on the OPPOSITE side of that seam.
fn link_seams(polys: &mut [Poly], verts: &[[f32; 3]], lat: SeamLattice) {
    if lat.tile_w <= 0.0 {
        return;
    }
    // axis 0 = vertical seam (x = const), interval runs along z; axis 1 = horizontal.
    struct SeamEdge {
        poly: usize,
        slot: usize,
        seam_key: (u8, i64),
        t0: f32,
        t1: f32,
        y: f32,
        side: bool,
    }
    let centroid = |poly: &Poly| -> [f32; 2] {
        let mut cx = 0.0;
        let mut cz = 0.0;
        for &vi in &poly.verts {
            let w = verts[vi as usize];
            cx += w[0];
            cz += w[2];
        }
        let n = poly.verts.len() as f32;
        [cx / n, cz / n]
    };
    let cents: Vec<[f32; 2]> = polys.iter().map(centroid).collect();

    let mut seam_edges: Vec<SeamEdge> = Vec::new();
    for (pi, poly) in polys.iter().enumerate() {
        let m = poly.verts.len();
        for j in 0..m {
            if poly.neighbors[j] >= 0 {
                continue;
            }
            let a = verts[poly.verts[j] as usize];
            let b = verts[poly.verts[(j + 1) % m] as usize];
            if let (Some(sx), true) = (lat.line_coord(0, a[0]), lat.line_coord(0, b[0]).is_some()) {
                let key = (0u8, (sx / lat.cs).round() as i64);
                seam_edges.push(SeamEdge {
                    poly: pi,
                    slot: j,
                    seam_key: key,
                    t0: a[2].min(b[2]),
                    t1: a[2].max(b[2]),
                    y: (a[1] + b[1]) * 0.5,
                    side: cents[pi][0] < sx,
                });
            } else if let (Some(sz), true) =
                (lat.line_coord(1, a[2]), lat.line_coord(1, b[2]).is_some())
            {
                let key = (1u8, (sz / lat.cs).round() as i64);
                seam_edges.push(SeamEdge {
                    poly: pi,
                    slot: j,
                    seam_key: key,
                    t0: a[0].min(b[0]),
                    t1: a[0].max(b[0]),
                    y: (a[1] + b[1]) * 0.5,
                    side: cents[pi][1] < sz,
                });
            }
        }
    }

    // For each still-unlinked seam edge, link to the opposite-side seam edge on the
    // same seam line with the largest interval overlap and a close Y (one neighbour
    // per edge slot; directional links suffice for A* reachability and become
    // bidirectional when both edges pick each other).
    // Two tiles partition the shared seam line with DIFFERENT vertex breakpoints, so
    // opposite-side edges mostly sit end-to-end (touching/offset) rather than exactly
    // overlapping. Link each edge to the opposite-side edge with the best interval
    // proximity (overlap, or a gap up to one cell) and a close Y. One neighbour per
    // slot is enough for A* reachability; bidirectional emerges when both pick each other.
    let max_gap = lat.cs;
    let max_dy = 1.0_f32;
    for i in 0..seam_edges.len() {
        let e = &seam_edges[i];
        if polys[e.poly].neighbors[e.slot] >= 0 {
            continue;
        }
        let mut best: Option<(usize, f32)> = None;
        for (k, o) in seam_edges.iter().enumerate() {
            if k == i || o.poly == e.poly || o.seam_key != e.seam_key || o.side == e.side {
                continue;
            }
            // positive = overlap length; negative = gap between the intervals.
            let prox = e.t1.min(o.t1) - e.t0.max(o.t0);
            if prox <= -max_gap || (o.y - e.y).abs() > max_dy {
                continue;
            }
            if best.is_none_or(|(_, bp)| prox > bp) {
                best = Some((k, prox));
            }
        }
        if let Some((k, _)) = best {
            polys[e.poly].neighbors[e.slot] = seam_edges[k].poly as i32;
        }
    }
}

/// Native-tiling seam linker (the default). Uses rerecast's PORTAL-edge markers
/// (`portal_dirs`) instead of a geometric lattice-line guess: an edge is a cross-tile
/// seam IFF the tiling path tagged it `0x8000 | dir`. After `rebuild_adjacency` has
/// already linked every portal edge whose welded verts exactly coincide with the
/// opposite tile's (the common case — tiles share one global grid + see the same
/// border-overlap geometry), the only edges left are the rare ones the per-tile
/// contour simplifier split into non-coincident segments. Each is linked to the
/// best-overlapping portal edge of the OPPOSITE direction on the SAME seam line
/// (Detour's `connectExtLinks` overlap contract). Direction pairing (0=west/x.min,
/// 1=north/z.max, 2=east/x.max, 3=south/z.min) guarantees we never link two edges on
/// the same side, so a real interior wall (its edges are SOLID, not portals) is
/// untouchable.
fn link_portals(polys: &mut [Poly], verts: &[[f32; 3]], portal_dirs: &[Vec<i8>], cs: f32) {
    struct PortalEdge {
        poly: usize,
        slot: usize,
        dir: i8,
        // Seam line coordinate (x for west/east, z for north/south), quantised to cells.
        line: i64,
        // Interval along the seam line.
        t0: f32,
        t1: f32,
        y: f32,
    }

    let mut edges: Vec<PortalEdge> = Vec::new();
    for (pi, poly) in polys.iter().enumerate() {
        let m = poly.verts.len();
        let pd = &portal_dirs[pi];
        for j in 0..m {
            if poly.neighbors[j] >= 0 {
                continue;
            }
            let dir = pd.get(j).copied().unwrap_or(-1);
            if dir < 0 {
                continue;
            }
            let a = verts[poly.verts[j] as usize];
            let b = verts[poly.verts[(j + 1) % m] as usize];
            // west/east run along z (line = x); north/south run along x (line = z).
            let (line, t0, t1) = if dir == 0 || dir == 2 {
                (
                    ((a[0] + b[0]) * 0.5 / cs).round() as i64,
                    a[2].min(b[2]),
                    a[2].max(b[2]),
                )
            } else {
                (
                    ((a[2] + b[2]) * 0.5 / cs).round() as i64,
                    a[0].min(b[0]),
                    a[0].max(b[0]),
                )
            };
            edges.push(PortalEdge {
                poly: pi,
                slot: j,
                dir,
                line,
                t0,
                t1,
                y: (a[1] + b[1]) * 0.5,
            });
        }
    }

    let opposite = |d: i8| -> i8 {
        match d {
            0 => 2,
            2 => 0,
            1 => 3,
            _ => 1,
        }
    };
    let max_dy = 1.0_f32;
    for i in 0..edges.len() {
        let e = &edges[i];
        if polys[e.poly].neighbors[e.slot] >= 0 {
            continue;
        }
        let want = opposite(e.dir);
        let mut best: Option<(usize, f32)> = None;
        for (k, o) in edges.iter().enumerate() {
            if k == i || o.poly == e.poly || o.dir != want || o.line != e.line {
                continue;
            }
            let overlap = e.t1.min(o.t1) - e.t0.max(o.t0);
            // Require genuine interval overlap (non-coincident segments still share a
            // sub-interval); a tiny tolerance covers the rare 1-cell simplifier offset.
            if overlap <= -cs || (o.y - e.y).abs() > max_dy {
                continue;
            }
            if best.is_none_or(|(_, bo)| overlap > bo) {
                best = Some((k, overlap));
            }
        }
        if let Some((k, _)) = best {
            let (op, os) = (edges[k].poly, edges[k].slot);
            polys[e.poly].neighbors[e.slot] = op as i32;
            if polys[op].neighbors[os] < 0 {
                polys[op].neighbors[os] = e.poly as i32;
            }
        }
    }
}

/// The tile-seam lattice geometry threaded through every seam linker: interior
/// seam lines run at `origin[axis] + k*tile_w` (k >= 1) on both axes, with the
/// cell size `cs` as the snap-tolerance quantum. Read-only.
#[derive(Clone, Copy)]
struct SeamLattice {
    origin: [f32; 2],
    tile_w: f32,
    cs: f32,
}

impl SeamLattice {
    /// `Some(k)` if `coord` lies within `cs*1.5` of an interior lattice line
    /// `origin[axis] + k*tile_w` (k >= 1; the outermost map edge at k=0 is excluded).
    fn index(&self, axis: usize, coord: f32) -> Option<i64> {
        let o = self.origin[axis];
        let k = ((coord - o) / self.tile_w).round();
        if k < 0.5 {
            return None;
        }
        let s = o + k * self.tile_w;
        ((coord - s).abs() <= self.cs * 1.5).then_some(k as i64)
    }

    /// Snap `coord` to its interior lattice line, returning the line coordinate.
    fn line_coord(&self, axis: usize, coord: f32) -> Option<f32> {
        self.index(axis, coord)
            .map(|k| self.origin[axis] + k as f32 * self.tile_w)
    }

    /// `Some((axis, k))` if both endpoints lie on the SAME interior lattice line
    /// (axis 0 = const-X, axis 1 = const-Z) — a cross-tile seam edge. Purely geometric,
    /// so it catches collinear seam edges the per-tile contour simplifier left unmarked
    /// as portals. A real interior wall is never exactly on a `k*tile_w` line.
    fn edge_seam(&self, verts: &[[f32; 3]], va: u32, vb: u32) -> Option<(u8, i64)> {
        let a = verts[va as usize];
        let b = verts[vb as usize];
        if let (Some(ka), Some(kb)) = (self.index(0, a[0]), self.index(0, b[0])) {
            if ka == kb {
                return Some((0, ka));
            }
        }
        if let (Some(ka), Some(kb)) = (self.index(1, a[2]), self.index(1, b[2])) {
            if ka == kb {
                return Some((1, ka));
            }
        }
        None
    }
}

/// Record, for each global vertex that is an endpoint of a PORTAL-marked edge, which
/// seam line(s) it lies on (axis 0 = const-X, axis 1 = const-Z; a 4-tile-corner vert
/// gets both). Portal markers are rerecast's ground truth for cross-tile boundaries,
/// so this never picks up an interior wall.
fn mark_seam_verts(
    polys: &[Poly],
    verts: &[[f32; 3]],
    portal_dirs: &[Vec<i8>],
    lat: SeamLattice,
    vert_seam: &mut [[Option<i64>; 2]],
) {
    for s in vert_seam.iter_mut() {
        *s = [None, None];
    }
    for (pi, poly) in polys.iter().enumerate() {
        let m = poly.verts.len();
        for j in 0..m {
            let dir = portal_dirs[pi].get(j).copied().unwrap_or(-1);
            let axis: u8 = match dir {
                0 | 2 => 0,
                1 | 3 => 1,
                _ => continue,
            };
            for &vi in [poly.verts[j], poly.verts[(j + 1) % m]].iter() {
                let w = verts[vi as usize];
                let coord = if axis == 0 { w[0] } else { w[2] };
                if let Some(k) = lat.index(axis as usize, coord) {
                    vert_seam[vi as usize][axis as usize] = Some(k);
                }
            }
        }
    }
}

/// T-junction resolution (the default native seam linker). Adjacent tiles' contour
/// simplifiers pick DIFFERENT vertex breakpoints along a shared seam line, so opposite
/// portal edges are collinear but their verts don't coincide and the exact undirected
/// edge-match can't link them. This makes every seam vertex exist on BOTH sides so the
/// rebuilt adjacency links each sub-segment with no heuristic:
///   1. WELD on-seam verts by their along-seam coordinate (snap coincident-along-seam
///      verts from opposite tiles to ONE position incl Y), clustering by Y so
///      vertically-stacked verts never merge.
///   2. INJECT breakpoints: for each portal edge, split it at any seam vertex strictly
///      interior to it, growing the poly's vert/neighbour loops.
///
/// `rebuild_adjacency` then bilaterally links every seam sub-edge. Interior walls stay
/// SOLID (never portals) so nothing punches through.
fn inject_seam_breakpoints(
    polys: &mut [Poly],
    verts: &mut [[f32; 3]],
    portal_dirs: &mut [Vec<i8>],
    lat: SeamLattice,
    ch: f32,
) {
    if lat.tile_w <= 0.0 {
        return;
    }
    // For each global vertex on a portal seam edge, record which seam line it lies on.
    // A vertex may sit at a 4-tile corner (on both axes); record up to both.
    let mut vert_seam: Vec<[Option<i64>; 2]> = vec![[None, None]; verts.len()];
    mark_seam_verts(polys, verts, portal_dirs, lat, &mut vert_seam);

    // --- Step 1: weld coincident-along-seam verts to ONE representative (incl Y). ---
    // Bucket on-seam verts by (axis, lattice, along-coordinate quantized to 0.1*cs),
    // then split each bucket into Y clusters (so vertically-stacked verts stay distinct)
    // and snap each cluster to its centroid.
    let aq = (lat.cs * 0.1).max(1e-4);
    let mut buckets: std::collections::HashMap<(u8, i64, i64), Vec<u32>> =
        std::collections::HashMap::new();
    for (vi, seams) in vert_seam.iter().enumerate() {
        for axis in 0u8..2 {
            if let Some(k) = seams[axis as usize] {
                let w = verts[vi];
                let along = if axis == 0 { w[2] } else { w[0] };
                let aqi = (along / aq).round() as i64;
                buckets.entry((axis, k, aqi)).or_default().push(vi as u32);
            }
        }
    }
    let mut remap: Vec<u32> = (0..verts.len() as u32).collect();
    let y_tol = (ch * 2.0).max(0.5);
    for ids in buckets.values() {
        if ids.len() < 2 {
            continue;
        }
        let mut sorted = ids.clone();
        sorted.sort_by(|&a, &b| verts[a as usize][1].total_cmp(&verts[b as usize][1]));
        let mut cluster: Vec<u32> = Vec::new();
        let flush = |cluster: &mut Vec<u32>, verts: &mut [[f32; 3]], remap: &mut [u32]| {
            if cluster.len() < 2 {
                cluster.clear();
                return;
            }
            let n = cluster.len() as f32;
            let mut c = [0.0f32; 3];
            for &id in cluster.iter() {
                let v = verts[id as usize];
                for (ck, vk) in c.iter_mut().zip(v) {
                    *ck += vk;
                }
            }
            for ck in c.iter_mut() {
                *ck /= n;
            }
            let rep = cluster[0];
            verts[rep as usize] = c;
            for &id in cluster.iter() {
                remap[id as usize] = rep;
                verts[id as usize] = c;
            }
            cluster.clear();
        };
        for &id in &sorted {
            if let Some(&last) = cluster.last() {
                if (verts[id as usize][1] - verts[last as usize][1]).abs() > y_tol {
                    flush(&mut cluster, verts, &mut remap);
                }
            }
            cluster.push(id);
        }
        flush(&mut cluster, verts, &mut remap);
    }
    if remap.iter().enumerate().any(|(i, &r)| r != i as u32) {
        for poly in polys.iter_mut() {
            for vi in poly.verts.iter_mut() {
                *vi = remap[*vi as usize];
            }
        }
        // Drop now-degenerate consecutive duplicates a weld may have introduced.
        for (pi, poly) in polys.iter_mut().enumerate() {
            let m = poly.verts.len();
            if m < 3 {
                continue;
            }
            let mut nv = Vec::with_capacity(m);
            let mut npd = Vec::with_capacity(m);
            for j in 0..m {
                let cur = poly.verts[j];
                let next = poly.verts[(j + 1) % m];
                if cur == next {
                    continue;
                }
                nv.push(cur);
                npd.push(portal_dirs[pi].get(j).copied().unwrap_or(-1));
            }
            if nv.len() >= 3 {
                poly.neighbors = vec![-1; nv.len()];
                poly.verts = nv;
                portal_dirs[pi] = npd;
            }
        }
        // Rebuild vert_seam against the welded representative set.
        mark_seam_verts(polys, verts, portal_dirs, lat, &mut vert_seam);
    }

    // --- Step 2: collect per-seam sorted breakpoint verts. ---
    let seam_verts = collect_seam_breakpoints(&vert_seam, verts);

    // --- Step 3: inject interior seam verts into each border seam edge. ---
    inject_breakpoints_into_edges(polys, verts, portal_dirs, lat, &seam_verts, aq);
}

/// Collect, for each seam line (axis, lattice-index), the sorted+deduped list of
/// `(along-seam-coordinate, global-vert-index)` breakpoint candidates lying on it.
fn collect_seam_breakpoints(
    vert_seam: &[[Option<i64>; 2]],
    verts: &[[f32; 3]],
) -> std::collections::HashMap<(u8, i64), Vec<(f32, u32)>> {
    // seam key (axis, lattice) -> sorted unique (along-coord, vert-index) list.
    let mut seam_verts: std::collections::HashMap<(u8, i64), Vec<(f32, u32)>> =
        std::collections::HashMap::new();
    for (vi, seams) in vert_seam.iter().enumerate() {
        for axis in 0u8..2 {
            if let Some(k) = seams[axis as usize] {
                let w = verts[vi];
                let along = if axis == 0 { w[2] } else { w[0] };
                seam_verts
                    .entry((axis, k))
                    .or_default()
                    .push((along, vi as u32));
            }
        }
    }
    for list in seam_verts.values_mut() {
        list.sort_by(|a, b| a.0.total_cmp(&b.0));
        list.dedup_by_key(|p| p.1);
    }
    seam_verts
}

/// Inject interior seam verts into each border seam edge, splitting it at any seam
/// vertex strictly interior to it and growing the poly's vert/neighbour loops.
fn inject_breakpoints_into_edges(
    polys: &mut [Poly],
    verts: &[[f32; 3]],
    portal_dirs: &mut [Vec<i8>],
    lat: SeamLattice,
    seam_verts: &std::collections::HashMap<(u8, i64), Vec<(f32, u32)>>,
    split_tol: f32,
) {
    for (pi, poly) in polys.iter_mut().enumerate() {
        let mut j = 0;
        while j < poly.verts.len() {
            let m = poly.verts.len();
            if poly.neighbors[j] >= 0 {
                j += 1;
                continue;
            }
            let va = poly.verts[j];
            let vb = poly.verts[(j + 1) % m];
            let dir = portal_dirs[pi].get(j).copied().unwrap_or(-1);
            let (axis, k) = match (dir, lat.edge_seam(verts, va, vb)) {
                (0..=3, Some(s)) => s,
                _ => {
                    j += 1;
                    continue;
                }
            };
            let wa = verts[va as usize];
            let wb = verts[vb as usize];
            let along_a = if axis == 0 { wa[2] } else { wa[0] };
            let along_b = if axis == 0 { wb[2] } else { wb[0] };
            let (lo, hi) = (along_a.min(along_b), along_a.max(along_b));
            // First seam vertex strictly interior to (lo,hi) on this seam line.
            let mut insert: Option<u32> = None;
            if let Some(list) = seam_verts.get(&(axis, k)) {
                for &(along, vi) in list {
                    if vi == va || vi == vb {
                        continue;
                    }
                    if along > lo + split_tol && along < hi - split_tol {
                        insert = Some(vi);
                        break;
                    }
                }
            }
            match insert {
                Some(vi) => {
                    let pos = (j + 1) % poly.verts.len();
                    if pos == 0 {
                        poly.verts.push(vi);
                        poly.neighbors.push(-1);
                        portal_dirs[pi].push(dir);
                    } else {
                        poly.verts.insert(pos, vi);
                        poly.neighbors.insert(pos, -1);
                        portal_dirs[pi].insert(pos, dir);
                    }
                    // Re-test edge j (now ending at the new vert) for further splits.
                }
                None => j += 1,
            }
        }
    }
}

fn rebuild_adjacency(polys: &mut [Poly]) {
    let mut edges: std::collections::HashMap<(u32, u32), Vec<(usize, usize)>> =
        std::collections::HashMap::new();
    for (pi, poly) in polys.iter().enumerate() {
        let m = poly.verts.len();
        for j in 0..m {
            let a = poly.verts[j];
            let b = poly.verts[(j + 1) % m];
            if a == b {
                continue;
            }
            let ek = (a.min(b), a.max(b));
            edges.entry(ek).or_default().push((pi, j));
        }
    }
    for owners in edges.values() {
        if owners.len() != 2 {
            continue;
        }
        let (p, jp) = owners[0];
        let (q, jq) = owners[1];
        polys[p].neighbors[jp] = q as i32;
        polys[q].neighbors[jq] = p as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_rerecast;

    fn flat_grid(n: usize, step: f32) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
        let mut verts = Vec::new();
        for iz in 0..=n {
            for ix in 0..=n {
                verts.push([ix as f32 * step, 0.0, iz as f32 * step]);
            }
        }
        let w = n + 1;
        let mut tris = Vec::new();
        for iz in 0..n {
            for ix in 0..n {
                let a = (iz * w + ix) as u32;
                let b = (iz * w + ix + 1) as u32;
                let c = ((iz + 1) * w + ix) as u32;
                let d = ((iz + 1) * w + ix + 1) as u32;
                tris.push([a, c, b]);
                tris.push([b, c, d]);
            }
        }
        (verts, tris)
    }

    fn components(nav: &PolyNavmesh) -> (usize, bool) {
        let real: Vec<bool> = nav.polys.iter().map(|p| p.verts.len() >= 3).collect();
        let n = nav.polys.len();
        let mut comp = vec![usize::MAX; n];
        let mut ncomp = 0;
        for s in 0..n {
            if !real[s] || comp[s] != usize::MAX {
                continue;
            }
            let mut stack = vec![s];
            comp[s] = ncomp;
            while let Some(p) = stack.pop() {
                for &nb in &nav.polys[p].neighbors {
                    if nb >= 0 {
                        let nb = nb as usize;
                        if real[nb] && comp[nb] == usize::MAX {
                            comp[nb] = ncomp;
                            stack.push(nb);
                        }
                    }
                }
            }
            ncomp += 1;
        }
        let all_reached = real.iter().zip(&comp).all(|(&r, &c)| !r || c != usize::MAX);
        (ncomp, all_reached)
    }

    #[test]
    fn tiled_matches_single_tile_connectivity() {
        let (verts, tris) = flat_grid(30, 2.0);
        let cfg = BuildConfig::default();

        let single = build_rerecast(&verts, &tris, &[], &[], &cfg).unwrap();
        let (sc, sok) = components(&single);
        assert!(sok, "single-tile build left unreachable polys");
        assert_eq!(
            sc, 1,
            "synthetic flat terrain should be one component (single)"
        );

        for mode in [MergeMode::Inject, MergeMode::Portal, MergeMode::Weld] {
            let tiled = build_tiled(&verts, &tris, &[], &[], &cfg, 20.0, mode).unwrap();
            let (tc, tok) = components(&tiled);
            assert!(tok, "{mode:?}: tiled build left unreachable polys");
            assert_eq!(
                tc, 1,
                "{mode:?}: tiled build must not split flat terrain at seams"
            );
        }
    }
}
