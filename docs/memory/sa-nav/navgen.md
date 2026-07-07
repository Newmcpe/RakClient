# sa-nav/navgen.rs

## Module overview
<anchor: module-overview>

`navgen` — build an on-foot navmesh for a region of the SA world and write it
as a `.nav` file (SA-space, see `sa_nav::NavMesh`).

Usage:
  navgen <gta3.img> <data-dir> <out.nav> [objects.csv] [cx cy half]

The world is assembled exactly like the viewer (collision + streamer bin +
IPLs, plus the streamed-objects CSV overlay if given), culled to the region
square `[cx±half, cy±half]` (SA x/y), converted to Y-up, and fed through the
tiled Recast pipeline. Everything but the largest connected component (roofs,
fenced pockets) is dropped — the bot can only start from the ground.

---

## cull_region — winding normalisation
<anchor: cull-region>

Keep triangles whose SA-xy bbox intersects the region square; compact the verts
and convert each to recast Y-up space.

Winding is NORMALISED to face up: SA collision meshes carry no winding contract
(the engine treats them double-sided), but rerecast's walkable-slope test reads
the SIGNED +Y normal — a ground triangle wound "down" reads non-walkable and
punches a hole, fragmenting the terrain into hundreds of pockets (observed:
71 components on a flat 400 m region before this).

---
