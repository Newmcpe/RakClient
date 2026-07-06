#ifndef RECAST_WRAPPER_H
#define RECAST_WRAPPER_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct RcConfigC {
    float cell_size;            // xz voxel size (m)
    float cell_height;          // y voxel size (m)
    float walkable_slope_angle; // max climbable slope (deg)
    float agent_height;         // tank height / clearance (m)
    float agent_radius;         // tank half-width (m)
    float agent_max_climb;      // max step/ledge a tank climbs (m)
    float edge_max_len;         // max contour edge length (m)
    float edge_max_error;       // contour simplification error (vx)
    float region_min_size;      // min region size (sqrt of cells)
    float region_merge_size;    // merge region size (sqrt of cells)
    float detail_sample_dist;   // detail sample distance (cs multiples); <0.9 disables
    float detail_sample_max_error; // detail max error (ch multiples)
} RcConfigC;

// Polygon navmesh result, world-space. polys layout per poly: nvp vertex indices
// (65535 = unused) followed by nvp neighbour-poly indices (65535 = solid/border edge).
//
// The DETAIL mesh (dverts/dtris) is a triangulation of the same walkable surface
// at ~detailSampleDist spacing that follows the terrain height (the convex polys
// are flat plane fits; the detail tris hug the ground). It is the source for the
// runtime 1-unit triangle navmesh. dtris hold flat global indices into dverts.
typedef struct RcResultC {
    float* verts;          // nverts * 3 (x,y,z) world
    int nverts;
    int* polys;            // npolys * (nvp*2)
    int npolys;
    int nvp;               // max verts per poly
    unsigned char* areas;  // npolys
    float* dverts;         // ndverts * 3 (x,y,z) world  — detail mesh vertices
    int ndverts;
    int* dtris;            // ndtris * 3 — detail triangles (global indices into dverts)
    int ndtris;
    int* dmeshes;          // ndmeshes * 2 — per-poly (first_global_tri, tri_count) into dtris
    int ndmeshes;          // == npolys (one detail submesh per convex poly, in order)
    int ok;
    char err[128];
} RcResultC;

// tri_obstacle (may be NULL): per-triangle flag; 1 = obstacle (rasterized solid but
// never walkable, e.g. building walls/roofs), 0 = surface (walkable if slope allows).
RcResultC* recast_build(const float* verts, int nverts,
                        const int* tris, int ntris,
                        const unsigned char* tri_obstacle,
                        const RcConfigC* cfg);
void recast_free(RcResultC* r);

#ifdef __cplusplus
}
#endif

#endif
