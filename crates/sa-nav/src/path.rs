//! Runtime pathfinding over a loaded [`NavMesh`]: locate the polygons under two
//! SA-space points, A* across the per-edge adjacency, and return waypoints
//! (shared-edge midpoints + the exact target). Midpoint waypoints keep v1 simple
//! and robust; a funnel/string-pulling pass can smooth them later — the walker
//! already moves point-to-point, so smoother paths only shorten travel, never
//! change correctness.

use std::collections::BinaryHeap;

use crate::NavMesh;

/// Pathfinding view over a navmesh: precomputed poly centroids + queries.
pub struct NavQuery {
    mesh: NavMesh,
    centers: Vec<[f32; 3]>,
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

    /// The polygon containing `(x, y)` in SA-XY whose plane is nearest `z`
    /// (within ±4 m), or — off-mesh — the poly with the closest centroid within
    /// 10 m (a bot standing on a doorstep pebble still gets a path).
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
            for (pi, c) in self.centers.iter().enumerate() {
                if self.mesh.polys[pi].verts.len() < 3 {
                    continue;
                }
                let d = dist(*c, p);
                if d <= 10.0 && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((pi, d));
                }
            }
        }
        best.map(|(pi, _)| pi)
    }

    /// Waypoints from `from` to `to` (SA space): shared-edge midpoints of the A*
    /// poly corridor, ending in the exact `to`. `None` if either point is off the
    /// mesh or no corridor connects them. Single-poly paths return just `[to]`.
    pub fn find_path(&self, from: [f32; 3], to: [f32; 3]) -> Option<Vec<[f32; 3]>> {
        let start = self.locate(from)?;
        let goal = self.locate(to)?;
        let corridor = self.astar(start, goal)?;
        let mut out = Vec::with_capacity(corridor.len());
        for w in corridor.windows(2) {
            out.push(self.shared_edge_midpoint(w[0], w[1])?);
        }
        out.push(to);
        Some(out)
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

    /// Midpoint of the edge of `a` whose neighbour is `b`.
    fn shared_edge_midpoint(&self, a: usize, b: usize) -> Option<[f32; 3]> {
        let poly = &self.mesh.polys[a];
        let m = poly.verts.len();
        for j in 0..m {
            if poly.neighbors.get(j).copied() == Some(b as i32) {
                let va = self.mesh.verts[poly.verts[j] as usize];
                let vb = self.mesh.verts[poly.verts[(j + 1) % m] as usize];
                return Some([
                    (va[0] + vb[0]) * 0.5,
                    (va[1] + vb[1]) * 0.5,
                    (va[2] + vb[2]) * 0.5,
                ]);
            }
        }
        None
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
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
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
}
