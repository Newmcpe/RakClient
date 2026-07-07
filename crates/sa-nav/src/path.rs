//! Runtime pathfinding over a loaded [`NavMesh`] (locate → A* → funnel string-pull → ground); see docs/memory/sa-nav/path.md#module-overview

use std::collections::BinaryHeap;

use crate::NavMesh;

/// Max Z gap between a path segment's straight line and the terrain under its midpoint
/// before we subdivide and lift an intermediate waypoint onto the ground (metres).
const GROUND_Z_EPS: f32 = 0.3;
/// Don't subdivide a segment shorter than this in XY (metres) — below it the terrain
/// deviation is negligible and further splits just bloat the path.
const MIN_GROUND_STEP: f32 = 1.0;
/// Recursion cap for segment grounding: 2^6 = up to 64 samples per original segment,
/// resolving a ~64 m run to ~1 m steps — well past any real sawmill leg.
const MAX_GROUND_DEPTH: u32 = 6;

/// Pathfinding view over a navmesh: precomputed poly centroids + queries.
pub struct NavQuery {
    mesh: NavMesh,
    centers: Vec<[f32; 3]>,
}

/// One gate of the A* corridor: the shared edge between two consecutive polys,
/// endpoints classified left/right relative to the travel direction, plus the
/// poly being left (whose detail mesh grounds waypoints on this portal).
struct Portal {
    left: [f32; 3],
    right: [f32; 3],
    poly: usize,
}

impl NavQuery {
    pub fn new(mesh: NavMesh) -> Self {
        let centers = mesh
            .polys
            .iter()
            .map(|p| {
                let mut c = [0.0f32; 3];
                for &vi in &p.verts {
                    let v = mesh.verts[vi as usize];
                    for k in 0..3 {
                        c[k] += v[k];
                    }
                }
                let n = p.verts.len().max(1) as f32;
                c.map(|x| x / n)
            })
            .collect();
        NavQuery { mesh, centers }
    }

    pub fn mesh(&self) -> &NavMesh {
        &self.mesh
    }

    /// Navmesh floor height at `p` (locate the poly under it, sample its detail mesh),
    /// or `None` off-mesh. Used by tooling to compare the walkable floor against raw
    /// collision height (spotting obstacles the bake failed to carve).
    pub fn floor(&self, p: [f32; 3]) -> Option<f32> {
        let pi = self.locate(p)?;
        self.ground_height(pi, p[0], p[1])
    }

    /// The polygon under `(x, y)` in SA-XY nearest `z`, else the nearest poly within a tight off-mesh radius; see docs/memory/sa-nav/path.md#locate
    pub fn locate(&self, p: [f32; 3]) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (pi, poly) in self.mesh.polys.iter().enumerate() {
            if poly.verts.len() < 3 || !self.contains_xy(pi, p[0], p[1]) {
                continue;
            }
            let dz = (self.centers[pi][2] - p[2]).abs();
            if dz <= 4.0 && best.is_none_or(|(_, bz)| dz < bz) {
                best = Some((pi, dz));
            }
        }
        if best.is_none() {
            for (pi, poly) in self.mesh.polys.iter().enumerate() {
                if poly.verts.len() < 3 || (self.centers[pi][2] - p[2]).abs() > 4.0 {
                    continue;
                }
                let d2 = self.poly_xy_dist2(pi, p[0], p[1]);
                if d2 <= 2.5 * 2.5 && best.is_none_or(|(_, bd)| d2 < bd) {
                    best = Some((pi, d2));
                }
            }
        }
        best.map(|(pi, _)| pi)
    }

    /// Waypoints from `from` to `to` (SA space): A* corridor + funnel string-pull, each grounded on the detail mesh; `None` if off-mesh/disconnected; see docs/memory/sa-nav/path.md#find-path
    pub fn find_path(&self, from: [f32; 3], to: [f32; 3]) -> Option<Vec<[f32; 3]>> {
        let start = self.locate(from)?;
        let goal = self.locate(to)?;
        // Clamp off-mesh endpoints onto their located polys: the off-mesh leg
        // then runs straight to the nearest boundary point instead of drawing
        // an unconstrained segment through geometry.
        let from_c = self.clamp_to_poly(start, from);
        let to_c = self.clamp_to_poly(goal, to);
        let corridor = self.astar(start, goal)?;

        // Portal list bracketed by degenerate start/end portals, SSFA-style.
        let mut portals = Vec::with_capacity(corridor.len() + 1);
        portals.push(Portal {
            left: from_c,
            right: from_c,
            poly: start,
        });
        for w in corridor.windows(2) {
            let (va, vb) = self.shared_edge(w[0], w[1])?;
            // Classify the edge endpoints left/right of the crossing direction.
            // Convex polys put the two centroids strictly on opposite sides of
            // the edge line, so the travel vector is never parallel to the edge.
            let d = [
                self.centers[w[1]][0] - self.centers[w[0]][0],
                self.centers[w[1]][1] - self.centers[w[0]][1],
            ];
            let mid = [(va[0] + vb[0]) * 0.5, (va[1] + vb[1]) * 0.5];
            let (left, right) = if d[0] * (va[1] - mid[1]) - d[1] * (va[0] - mid[0]) >= 0.0 {
                (va, vb)
            } else {
                (vb, va)
            };
            portals.push(Portal {
                left,
                right,
                poly: w[0],
            });
        }
        portals.push(Portal {
            left: to_c,
            right: to_c,
            poly: goal,
        });

        // String-pull, then stitch the waypoint list: every funnel corner plus
        // the point where each remaining portal is pierced, so every waypoint
        // gets a grounded Z from the poly it stands on (detail is per-poly).
        let corners = funnel(&portals);
        let mut out: Vec<[f32; 3]> = Vec::new();
        if !xy_close(from_c, from) {
            // Off-mesh start: enter the mesh at the clamp point first.
            out.push(from_c);
        }
        let mut last = from_c;
        let mut last_i = 0usize;
        for &(ci, corner) in &corners {
            self.push_crossings(&portals, last_i + 1, ci, last, corner, &mut out);
            let mut wp = corner;
            if let Some(portal) = portals.get(ci) {
                if let Some(z) = self.ground_height(portal.poly, wp[0], wp[1]) {
                    wp[2] = z;
                }
            }
            let prev = out.last().copied().unwrap_or(from_c);
            if !xy_close(wp, prev) {
                out.push(wp);
            }
            last = corner;
            last_i = ci;
        }
        self.push_crossings(
            &portals,
            last_i + 1,
            portals.len() - 1,
            last,
            to_c,
            &mut out,
        );
        match out.last_mut() {
            // The last leg may already end on the (clamped) target, e.g. pinned
            // on a corner vertex; overwrite so it lands there exactly.
            Some(p) if xy_close(*p, to_c) => *p = to_c,
            _ => out.push(to_c),
        }
        if !xy_close(to_c, to) {
            // Off-mesh target: leave the mesh at the clamp point, then hop the
            // final off-mesh stretch to the exact `to`.
            let mut dest = to;
            if let Some(z) = self.ground_height(goal, to[0], to[1]) {
                dest[2] = z;
            }
            out.push(dest);
        }
        // Ground the polyline to the terrain (funnel corners cut under/over detail humps); see docs/memory/sa-nav/path.md#find-path-grounding
        Some(self.ground_polyline(from_c, &out))
    }

    /// Insert grounded intermediate waypoints so no straight segment (from `start`, not emitted, then `pts`) cuts through/over the detail-mesh terrain; see docs/memory/sa-nav/path.md#ground-polyline
    fn ground_polyline(&self, start: [f32; 3], pts: &[[f32; 3]]) -> Vec<[f32; 3]> {
        let mut out = Vec::with_capacity(pts.len());
        let mut prev = start;
        for &p in pts {
            self.ground_segment(prev, p, 0, &mut out);
            out.push(p);
            prev = p;
        }
        out
    }

    /// Recursively subdivide `a -> b`, appending grounded midpoints (not `a`/`b`, which the caller owns) where terrain bulges from the straight line, stopping at [`MAX_GROUND_DEPTH`] or below [`MIN_GROUND_STEP`] in XY.
    fn ground_segment(&self, a: [f32; 3], b: [f32; 3], depth: u32, out: &mut Vec<[f32; 3]>) {
        if depth >= MAX_GROUND_DEPTH {
            return;
        }
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        if dx * dx + dy * dy < MIN_GROUND_STEP * MIN_GROUND_STEP {
            return;
        }
        let (mx, my) = ((a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5);
        let interp = (a[2] + b[2]) * 0.5;
        // Sample ground under the midpoint; the interpolated Z is the locate hint so we
        // pick the poly at this height, not one stacked above/below it.
        let Some(pi) = self.locate([mx, my, interp]) else {
            return;
        };
        let Some(gz) = self.ground_height(pi, mx, my) else {
            return;
        };
        if (gz - interp).abs() <= GROUND_Z_EPS {
            return;
        }
        let mid = [mx, my, gz];
        self.ground_segment(a, mid, depth + 1, out);
        out.push(mid);
        self.ground_segment(mid, b, depth + 1, out);
    }

    /// A* over poly adjacency; cost = centroid-to-centroid distance.
    fn astar(&self, start: usize, goal: usize) -> Option<Vec<usize>> {
        #[derive(PartialEq)]
        struct Open(f32, usize);
        impl Eq for Open {}
        impl Ord for Open {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                o.0.total_cmp(&self.0) // min-heap
            }
        }
        impl PartialOrd for Open {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        let n = self.mesh.polys.len();
        let mut g = vec![f32::INFINITY; n];
        let mut prev = vec![usize::MAX; n];
        let mut heap = BinaryHeap::new();
        g[start] = 0.0;
        heap.push(Open(dist(self.centers[start], self.centers[goal]), start));
        while let Some(Open(_, cur)) = heap.pop() {
            if cur == goal {
                let mut path = vec![goal];
                let mut at = goal;
                while at != start {
                    at = prev[at];
                    path.push(at);
                }
                path.reverse();
                return Some(path);
            }
            for &nb in &self.mesh.polys[cur].neighbors {
                let Ok(nb) = usize::try_from(nb) else {
                    continue;
                };
                if nb >= n {
                    continue;
                }
                let ng = g[cur] + dist(self.centers[cur], self.centers[nb]);
                if ng < g[nb] {
                    g[nb] = ng;
                    prev[nb] = cur;
                    heap.push(Open(ng + dist(self.centers[nb], self.centers[goal]), nb));
                }
            }
        }
        None
    }

    /// The endpoints, in `a`'s winding order, of the edge of `a` whose
    /// neighbour is `b`.
    fn shared_edge(&self, a: usize, b: usize) -> Option<([f32; 3], [f32; 3])> {
        let poly = &self.mesh.polys[a];
        let m = poly.verts.len();
        for j in 0..m {
            if poly.neighbors.get(j).copied() == Some(b as i32) {
                let va = self.mesh.verts[poly.verts[j] as usize];
                let vb = self.mesh.verts[poly.verts[(j + 1) % m] as usize];
                return Some((va, vb));
            }
        }
        None
    }

    /// Appends a grounded waypoint where segment `a -> b` pierces each of
    /// `portals[lo..hi]`, skipping points that duplicate the previous waypoint
    /// or the segment end (`b` is pushed by the caller).
    fn push_crossings(
        &self,
        portals: &[Portal],
        lo: usize,
        hi: usize,
        a: [f32; 3],
        b: [f32; 3],
        out: &mut Vec<[f32; 3]>,
    ) {
        for portal in portals.get(lo..hi).unwrap_or(&[]) {
            let Some(mut hit) = portal_crossing(a, b, portal) else {
                continue;
            };
            if let Some(z) = self.ground_height(portal.poly, hit[0], hit[1]) {
                hit[2] = z;
            }
            let prev = out.last().copied().unwrap_or(a);
            if !xy_close(hit, prev) && !xy_close(hit, b) {
                out.push(hit);
            }
        }
    }

    /// Terrain height on poly `pi`'s detail mesh at SA-XY `(x, y)`, or `None` if the poly has no detail tris; see docs/memory/sa-nav/path.md#ground-height
    fn ground_height(&self, pi: usize, x: f32, y: f32) -> Option<f32> {
        let &[first, count] = self.mesh.detail_meshes.get(pi)?;
        let tris = self
            .mesh
            .detail_tris
            .get(first as usize..(first + count) as usize)?;
        let mut nearest: Option<(f32, f32)> = None; // (dist2 to a vert, that vert's z)
        for t in tris {
            let a = self.mesh.detail_verts[t[0] as usize];
            let b = self.mesh.detail_verts[t[1] as usize];
            let c = self.mesh.detail_verts[t[2] as usize];
            if let Some(z) = tri_height(a, b, c, x, y) {
                return Some(z);
            }
            for v in [a, b, c] {
                let d2 = (v[0] - x) * (v[0] - x) + (v[1] - y) * (v[1] - y);
                if nearest.is_none_or(|(bd, _)| d2 < bd) {
                    nearest = Some((d2, v[2]));
                }
            }
        }
        nearest.map(|(_, z)| z)
    }

    /// Point-in-convex-polygon in SA XY, winding-agnostic (all cross products the
    /// same sign, zero tolerated for on-edge points).
    fn contains_xy(&self, pi: usize, x: f32, y: f32) -> bool {
        let poly = &self.mesh.polys[pi];
        let m = poly.verts.len();
        let mut sign = 0.0f32;
        for j in 0..m {
            let a = self.mesh.verts[poly.verts[j] as usize];
            let b = self.mesh.verts[poly.verts[(j + 1) % m] as usize];
            let cross = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
            if cross.abs() < 1e-6 {
                continue;
            }
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
        true
    }

    /// Squared XY distance from `(x, y)` to poly `pi`: 0 inside, else the
    /// squared distance to the nearest point on its boundary.
    fn poly_xy_dist2(&self, pi: usize, x: f32, y: f32) -> f32 {
        if self.contains_xy(pi, x, y) {
            return 0.0;
        }
        let (nx, ny) = self.poly_nearest_xy(pi, x, y);
        (nx - x) * (nx - x) + (ny - y) * (ny - y)
    }

    /// Nearest point to `(x, y)` on poly `pi`'s boundary in SA XY. Falls back
    /// to `(x, y)` itself for a poly with no edges (never queried in practice —
    /// callers filter out polys with fewer than 3 verts).
    fn poly_nearest_xy(&self, pi: usize, x: f32, y: f32) -> (f32, f32) {
        let poly = &self.mesh.polys[pi];
        let m = poly.verts.len();
        let mut best = (f32::INFINITY, (x, y));
        for j in 0..m {
            let a = self.mesh.verts[poly.verts[j] as usize];
            let b = self.mesh.verts[poly.verts[(j + 1) % m] as usize];
            let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
            let len2 = ex * ex + ey * ey;
            let t = if len2 > 1e-12 {
                (((x - a[0]) * ex + (y - a[1]) * ey) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (px, py) = (a[0] + t * ex, a[1] + t * ey);
            let d2 = (px - x) * (px - x) + (py - y) * (py - y);
            if d2 < best.0 {
                best = (d2, (px, py));
            }
        }
        best.1
    }

    /// `p` clamped onto poly `pi` in XY (unchanged when already inside), with Z
    /// re-grounded on the poly's detail mesh.
    fn clamp_to_poly(&self, pi: usize, p: [f32; 3]) -> [f32; 3] {
        let (x, y) = if self.contains_xy(pi, p[0], p[1]) {
            (p[0], p[1])
        } else {
            self.poly_nearest_xy(pi, p[0], p[1])
        };
        let z = self.ground_height(pi, x, y).unwrap_or(p[2]);
        [x, y, z]
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// Interpolated Z at SA-XY `(x, y)` on triangle `a,b,c`, or `None` if the point's
/// XY projection is outside the triangle. Barycentric in the XY plane; a small
/// epsilon admits on-edge points so a shared edge is covered by both sides.
fn tri_height(a: [f32; 3], b: [f32; 3], c: [f32; 3], x: f32, y: f32) -> Option<f32> {
    let (v0x, v0y) = (b[0] - a[0], b[1] - a[1]);
    let (v1x, v1y) = (c[0] - a[0], c[1] - a[1]);
    let (v2x, v2y) = (x - a[0], y - a[1]);
    let den = v0x * v1y - v1x * v0y;
    if den.abs() < 1e-9 {
        return None; // degenerate (zero-area) triangle in XY
    }
    let inv = 1.0 / den;
    let u = (v2x * v1y - v1x * v2y) * inv;
    let v = (v0x * v2y - v2x * v0y) * inv;
    let eps = 1e-3;
    if u >= -eps && v >= -eps && u + v <= 1.0 + eps {
        Some(a[2] + u * (b[2] - a[2]) + v * (c[2] - a[2]))
    } else {
        None
    }
}

/// Simple Stupid Funnel Algorithm (Mononen) over the corridor portals, emitting concave corner vertices as `(portal index, point)`; the destination is appended by the caller; see docs/memory/sa-nav/path.md#funnel
fn funnel(portals: &[Portal]) -> Vec<(usize, [f32; 3])> {
    let mut corners = Vec::new();
    let Some(first) = portals.first() else {
        return corners;
    };
    let mut apex = first.left;
    let (mut left, mut right) = (apex, apex);
    let (mut left_i, mut right_i) = (0usize, 0usize);
    let mut i = 1;
    while i < portals.len() {
        let (pl, pr) = (portals[i].left, portals[i].right);
        // Tighten the right side.
        if cross_xy(apex, right, pr) >= 0.0 {
            if xy_close(apex, right) || cross_xy(apex, left, pr) < 0.0 {
                right = pr;
                right_i = i;
            } else {
                // Right swept over left: the left point is a path corner;
                // restart the funnel from it.
                corners.push((left_i, left));
                apex = left;
                right = apex;
                right_i = left_i;
                i = left_i + 1;
                continue;
            }
        }
        // Tighten the left side.
        if cross_xy(apex, left, pl) <= 0.0 {
            if xy_close(apex, left) || cross_xy(apex, right, pl) > 0.0 {
                left = pl;
                left_i = i;
            } else {
                corners.push((right_i, right));
                apex = right;
                left = apex;
                left_i = right_i;
                i = right_i + 1;
                continue;
            }
        }
        i += 1;
    }
    corners
}

/// XY crossing of segment `a -> b` with a portal's edge: `None` when parallel
/// or when the crossing falls outside either segment. Z is interpolated along
/// the portal edge; callers re-ground it from the detail mesh.
fn portal_crossing(a: [f32; 3], b: [f32; 3], portal: &Portal) -> Option<[f32; 3]> {
    let (l, r) = (portal.left, portal.right);
    let d = [b[0] - a[0], b[1] - a[1]];
    let e = [r[0] - l[0], r[1] - l[1]];
    let den = d[0] * e[1] - d[1] * e[0];
    if den.abs() < 1e-9 {
        return None;
    }
    let w = [l[0] - a[0], l[1] - a[1]];
    let t = (w[0] * e[1] - w[1] * e[0]) / den; // along a -> b
    let s = (w[0] * d[1] - w[1] * d[0]) / den; // along left -> right
    let eps = 1e-3;
    if !(-eps..=1.0 + eps).contains(&t) || !(-eps..=1.0 + eps).contains(&s) {
        return None;
    }
    let (t, s) = (t.clamp(0.0, 1.0), s.clamp(0.0, 1.0));
    Some([a[0] + t * d[0], a[1] + t * d[1], l[2] + s * (r[2] - l[2])])
}

/// 2D cross product `(b - a) x (c - a)` in SA XY: positive when `c` is to the
/// left of `a -> b`.
fn cross_xy(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// XY coincidence within a millimetre — waypoint dedup and funnel apex checks.
fn xy_close(a: [f32; 3], b: [f32; 3]) -> bool {
    let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
    dx * dx + dy * dy < 1e-6
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NavPoly;

    /// Two unit-ish squares side by side sharing an edge: A x∈[0,10], B x∈[10,20].
    fn strip() -> NavMesh {
        NavMesh {
            verts: vec![
                [0.0, 0.0, 5.0],
                [10.0, 0.0, 5.0],
                [10.0, 10.0, 5.0],
                [0.0, 10.0, 5.0],
                [20.0, 0.0, 5.0],
                [20.0, 10.0, 5.0],
            ],
            polys: vec![
                NavPoly {
                    verts: vec![0, 1, 2, 3],
                    neighbors: vec![-1, 1, -1, -1],
                    area: 63,
                },
                NavPoly {
                    verts: vec![1, 4, 5, 2],
                    neighbors: vec![-1, -1, 0, -1],
                    area: 63,
                },
            ],
            detail_verts: vec![],
            detail_tris: vec![],
            detail_meshes: vec![[0, 0], [0, 0]],
        }
    }

    #[test]
    fn locates_and_paths_across_shared_edge() {
        let q = NavQuery::new(strip());
        assert_eq!(q.locate([2.0, 5.0, 5.0]), Some(0));
        assert_eq!(q.locate([18.0, 5.0, 5.0]), Some(1));
        assert_eq!(q.locate([500.0, 5.0, 5.0]), None);

        let path = q.find_path([2.0, 5.0, 5.0], [18.0, 6.0, 5.0]).unwrap();
        assert_eq!(path.len(), 2, "edge midpoint + target");
        assert_eq!(path[0][0], 10.0, "crossing at the shared edge x=10");
        assert_eq!(path[1], [18.0, 6.0, 5.0]);
    }

    #[test]
    fn same_poly_path_is_direct() {
        let q = NavQuery::new(strip());
        let path = q.find_path([2.0, 2.0, 5.0], [8.0, 8.0, 5.0]).unwrap();
        assert_eq!(path, vec![[8.0, 8.0, 5.0]]);
    }

    /// The coarse poly verts sit flat at z=5, but poly A's detail mesh slopes z=x
    /// (so the shared edge at x=10 is really at z=10). The crossing waypoint must be
    /// grounded to the detail height, not left at the flat coarse z.
    #[test]
    fn waypoint_z_is_sampled_from_detail_mesh() {
        let mut mesh = strip();
        // Detail for poly A (x in [0,10]) with z = x across the square.
        mesh.detail_verts = vec![
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 10.0],
            [10.0, 10.0, 10.0],
            [0.0, 10.0, 0.0],
        ];
        mesh.detail_tris = vec![[0, 1, 2], [0, 2, 3]];
        // Poly A owns both tris; poly B has none (so its target keeps the caller z).
        mesh.detail_meshes = vec![[0, 2], [2, 0]];

        let q = NavQuery::new(mesh);
        let path = q.find_path([2.0, 5.0, 5.0], [18.0, 5.0, 5.0]).unwrap();
        assert_eq!(path[0][0], 10.0, "crossing still at the shared edge x=10");
        assert!(
            (path[0][2] - 10.0).abs() < 1e-3,
            "crossing z grounded to the detail slope (z=x -> 10), got {}",
            path[0][2]
        );
    }

    /// An L-shaped corridor: A x∈[0,10]×y∈[0,10] → B x∈[10,20]×y∈[0,10] →
    /// C x∈[10,20]×y∈[10,20]. The quadrant x∈[0,10]×y∈[10,20] is void; vertex 2
    /// at (10, 10) is the inner corner of the L.
    fn l_corridor() -> NavMesh {
        NavMesh {
            verts: vec![
                [0.0, 0.0, 5.0],   // 0
                [10.0, 0.0, 5.0],  // 1
                [10.0, 10.0, 5.0], // 2: the inner corner
                [0.0, 10.0, 5.0],  // 3
                [20.0, 0.0, 5.0],  // 4
                [20.0, 10.0, 5.0], // 5
                [20.0, 20.0, 5.0], // 6
                [10.0, 20.0, 5.0], // 7
            ],
            polys: vec![
                NavPoly {
                    verts: vec![0, 1, 2, 3],
                    neighbors: vec![-1, 1, -1, -1],
                    area: 63,
                },
                NavPoly {
                    verts: vec![1, 4, 5, 2],
                    neighbors: vec![-1, -1, 2, 0],
                    area: 63,
                },
                NavPoly {
                    verts: vec![2, 5, 6, 7],
                    neighbors: vec![1, -1, -1, -1],
                    area: 63,
                },
            ],
            detail_verts: vec![],
            detail_tris: vec![],
            detail_meshes: vec![[0, 0], [0, 0], [0, 0]],
        }
    }

    /// The straight line (2,5) → (18,18) exits poly A through its top edge and
    /// re-enters B above y=10 — straight through the void quadrant, i.e. it
    /// cuts the inner corner. The funnel must pin the path to the inner corner
    /// vertex (10, 10) instead, and every waypoint must stay on the L.
    #[test]
    fn funnel_routes_through_inner_corner_of_l_corridor() {
        let q = NavQuery::new(l_corridor());
        let path = q.find_path([2.0, 5.0, 5.0], [18.0, 18.0, 5.0]).unwrap();
        assert_eq!(
            *path.last().unwrap(),
            [18.0, 18.0, 5.0],
            "path ends at the exact target"
        );
        assert!(
            path.iter()
                .any(|p| (p[0] - 10.0).abs() < 1e-3 && (p[1] - 10.0).abs() < 1e-3),
            "expected a waypoint pinned to the inner corner (10, 10), got {path:?}"
        );
        for p in &path {
            assert!(
                p[0] >= 10.0 - 1e-3 || p[1] <= 10.0 + 1e-3,
                "waypoint {p:?} is in the non-walkable quadrant"
            );
        }
    }

    /// `from` sits in the L's void quadrant, 1 m past poly A's top edge. The
    /// naive leg from the off-mesh point to the first corridor waypoint would
    /// cross the void; the clamp must instead enter the mesh at (8, 10) on A's
    /// boundary, and every waypoint must stay on the L.
    #[test]
    fn off_mesh_start_is_clamped_onto_the_poly() {
        let q = NavQuery::new(l_corridor());
        let path = q.find_path([8.0, 11.0, 5.0], [18.0, 5.0, 5.0]).unwrap();
        let first = path[0];
        assert!(
            (first[0] - 8.0).abs() < 1e-3 && (first[1] - 10.0).abs() < 1e-3,
            "first waypoint should be the clamp onto A's top edge at (8, 10), got {path:?}"
        );
        for p in &path {
            assert!(
                p[0] >= 10.0 - 1e-3 || p[1] <= 10.0 + 1e-3,
                "waypoint {p:?} is in the non-walkable quadrant"
            );
        }
        assert_eq!(*path.last().unwrap(), [18.0, 5.0, 5.0]);
    }

    /// One wide poly (x∈[0,40]) whose coarse verts are flat at z=0 but whose detail
    /// mesh humps up to z=8 at the centre (x=20): a straight path across it would cut
    /// ~8 m under the ridge. `ground_polyline` must subdivide and lift an intermediate
    /// waypoint onto the hump so the path follows the ground.
    #[test]
    fn grounds_segment_over_a_detail_hump() {
        let mesh = NavMesh {
            verts: vec![
                [0.0, 0.0, 0.0],
                [40.0, 0.0, 0.0],
                [40.0, 10.0, 0.0],
                [0.0, 10.0, 0.0],
            ],
            polys: vec![NavPoly {
                verts: vec![0, 1, 2, 3],
                neighbors: vec![-1, -1, -1, -1],
                area: 63,
            }],
            // A tent ridge along x=20: low at the ends (z=0), peak z=8 in the middle.
            detail_verts: vec![
                [0.0, 0.0, 0.0],
                [0.0, 10.0, 0.0],
                [20.0, 0.0, 8.0],
                [20.0, 10.0, 8.0],
                [40.0, 0.0, 0.0],
                [40.0, 10.0, 0.0],
            ],
            detail_tris: vec![[0, 2, 3], [0, 3, 1], [2, 4, 5], [2, 5, 3]],
            detail_meshes: vec![[0, 4]],
        };
        let q = NavQuery::new(mesh);
        let path = q.find_path([2.0, 5.0, 0.0], [38.0, 5.0, 0.0]).unwrap();
        // Somewhere near the ridge a waypoint must sit up on the hump (z well above the
        // flat straight-line 0), so the path no longer dives through the terrain.
        assert!(
            path.iter().any(|p| (p[0] - 20.0).abs() < 6.0 && p[2] > 5.0),
            "expected a grounded waypoint lifted onto the x=20 hump, got {path:?}"
        );
        let end = *path.last().unwrap();
        assert!(
            (end[0] - 38.0).abs() < 1e-3 && (end[1] - 5.0).abs() < 1e-3,
            "ends at target XY, got {end:?}"
        );
        // Target Z is grounded to the detail slope (z = 8·(40-38)/20 = 0.8), not left at 0.
        assert!(
            (end[2] - 0.8).abs() < 0.05,
            "target grounded to the hump slope, got {end:?}"
        );
    }

    /// The off-mesh fallback measures to the poly itself, not its centroid:
    /// 1 m off B's outer edge still locates (doorstep pebble), but 4 m off —
    /// only 9 m from B's centroid, inside the old 10 m centroid radius — must
    /// not.
    #[test]
    fn off_mesh_locate_uses_poly_distance_with_tight_radius() {
        let q = NavQuery::new(strip());
        assert_eq!(q.locate([21.0, 5.0, 5.0]), Some(1));
        assert_eq!(q.locate([24.0, 5.0, 5.0]), None);
    }
}
