# sa-nav/path.rs

## Module overview
<anchor: module-overview>

Runtime pathfinding over a loaded [`NavMesh`]: locate the polygons under two
SA-space points, A* across the per-edge adjacency, then pull the corridor
tight with the funnel algorithm (Mononen's Simple Stupid Funnel) so every
segment hugs the portal edges and stays inside the corridor polys instead of
zigzagging between shared-edge midpoints. Portal crossings are kept as
waypoints so each one can be grounded on its own poly's detail mesh.

---

## NavQuery::locate
<anchor: locate>

The polygon containing `(x, y)` in SA-XY whose plane is nearest `z`
(within ±4 m), or — off-mesh — the poly nearest by squared XY
point-to-poly distance within 2.5 m (and ±4 m in Z): a bot standing on
a doorstep pebble still gets a path, but a poly whose *centroid* is
near across a wall does not.

---

## NavQuery::find_path
<anchor: find-path>

Waypoints from `from` to `to` (SA space): the A* poly corridor pulled
tight with the funnel algorithm, plus a waypoint wherever the pulled
string pierces a portal, ending in the exact `to`. `None` if either
point is off the mesh or no corridor connects them. Single-poly paths
return just `[to]`.

Endpoints located via the off-mesh fallback are clamped onto their poly
and the clamp point is emitted as an extra first/last waypoint, so the
off-mesh leg is the shortest hop to the mesh boundary instead of an
unconstrained segment through geometry.

Each waypoint's Z is sampled from the terrain-following detail mesh, NOT the
coarse poly verts. The convex poly mesh quantizes vertex height to the cell
grid and fits a flat plane per poly, so on slopes its waypoints would sink
below the real ground — a path that visibly dives through the terrain and
makes the walker's Z jump. The detail tris hug the surface, so sampling them
keeps every waypoint on the ground.

---

## NavQuery::find_path — polyline grounding (inline)
<anchor: find-path-grounding>

Ground the polyline to the terrain. The funnel emits waypoints only at
corridor corners and portal crossings, so a straight segment between two of
them cuts UNDER a rise in the detail mesh (or floats over a dip) — a path
that visibly dives through the ground, which to other players reads as a
teleport/no-clip hack. Subdivide each segment and lift intermediate points
onto the detail surface wherever the straight line deviates from it. Planar
slopes need no inserts (ground == linear interp), so only real humps/dips add
waypoints. The walk starts at `from_c`, so ground the first leg from there too.

---

## NavQuery::ground_polyline
<anchor: ground-polyline>

Insert grounded intermediate waypoints so no straight segment of the walked
polyline (`start` then each of `pts`) cuts through (or floats over) the detail-
mesh terrain. Each segment is adaptively subdivided at its midpoint whenever the
sampled ground there departs from the segment's linear-interpolated Z by more
than [`GROUND_Z_EPS`]. `start` is the walk origin and is NOT emitted (the caller
already stands there); only `pts` and the grounded midpoints between them return.

---

## NavQuery::ground_height
<anchor: ground-height>

Terrain height on poly `pi`'s detail mesh at SA-XY `(x, y)`. Walks the poly's
detail-triangle slice (`detail_meshes[pi] = [first, count]` into `detail_tris`,
indices global into `detail_verts`) and barycentric-interpolates Z in the
triangle covering the point. Falls back to the nearest detail vertex's Z when
the point sits just outside every tri (edge midpoints can land a hair past the
border after simplification). `None` only if the poly carries no detail tris.

---

## funnel
<anchor: funnel>

Simple Stupid Funnel Algorithm (Mikko Mononen) over the corridor portals:
pull the string tight from the start portal's point, tightening the funnel
at each portal and emitting a corner — `(portal index it was pinned at,
point)` — whenever one side sweeps over the other. Corners are exactly the
concave corridor vertices a straight segment would otherwise cut through.
The destination itself is not emitted; the caller appends it.

---
