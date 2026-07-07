# sa-nav/lib.rs

## Module overview
<anchor: module-overview>

Navmesh generation + storage for the SA world, built on the local
`navmesh-recast` fork (pure-Rust Recast rewrite).

Coordinate contract: everything in THIS crate's public types is **SA space**
(Z-up, the coordinates the bot lives in). Recast wants Y-up; the conversion is
the same handedness-preserving mapping the viewer uses for Bevy:
`SA (x, y, z) -> recast (x, z, -y)` and back `recast (X, Y, Z) -> SA (X, -Z, Y)`.
Converting BEFORE the build (not after) matters: rerecast's walkable-slope test
reads the signed +Y component of each triangle normal, so both the axis swap
and the winding it implies must match.

The `.nav` file stores the flat convex-poly navmesh (verts + polys with
per-edge adjacency + areas) and the terrain-following detail triangles — the
polys drive A*, the detail tris drive height lookup and rendering.

---

## onfoot_config
<anchor: onfoot-config>

Build parameters for a SA-MP on-foot player agent.

Height ~1.8 m (SA ped), radius 0.35 m (capsule), climb 0.9 m (the engine steps
pedestrians up ~1 m ledges/stairs), slope 45° (peds walk hills the vanilla map
leans on heavily; steeper reads as cliff). Cell 0.25 m keeps doorways (~1 m)
and the sawmill's inter-object passages open after radius erosion of one cell.

---
