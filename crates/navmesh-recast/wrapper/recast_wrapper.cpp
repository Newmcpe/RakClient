// Thin C wrapper around the canonical Recast "solo mesh" build pipeline:
// triangle soup -> heightfield -> walkable filters -> compact heightfield ->
// erode by agent radius -> regions -> contours -> convex polygon mesh.
#include "Recast.h"
#include "recast_wrapper.h"

#include <cstdlib>
#include <cstring>
#include <cmath>

static RcResultC* fail(RcResultC* out, const char* msg) {
    if (out) {
        out->ok = 0;
        std::strncpy(out->err, msg, sizeof(out->err) - 1);
    }
    return out;
}

extern "C" RcResultC* recast_build(const float* verts, int nverts,
                                   const int* tris, int ntris,
                                   const unsigned char* tri_obstacle,
                                   const RcConfigC* c) {
    RcResultC* out = (RcResultC*)std::calloc(1, sizeof(RcResultC));
    if (!out) return out;
    if (nverts <= 0 || ntris <= 0) return fail(out, "empty input");

    rcContext ctx(false);

    rcConfig cfg;
    std::memset(&cfg, 0, sizeof(cfg));
    cfg.cs = c->cell_size;
    cfg.ch = c->cell_height;
    cfg.walkableSlopeAngle = c->walkable_slope_angle;
    cfg.walkableHeight = (int)std::ceil(c->agent_height / cfg.ch);
    // ROUND (not floor) the climb: floor(1.0/0.4)=floor(2.5)=2 silently makes the
    // effective step 0.8m, stricter than the runtime grid's 1.0m climb-sever and
    // below the intended tank step. lround(2.5)=3 -> 1.2m, honoring the intent.
    cfg.walkableClimb = (int)std::lround(c->agent_max_climb / cfg.ch);
    cfg.walkableRadius = (int)std::ceil(c->agent_radius / cfg.cs);
    cfg.maxEdgeLen = (int)(c->edge_max_len / cfg.cs);
    cfg.maxSimplificationError = c->edge_max_error;
    cfg.minRegionArea = (int)rcSqr(c->region_min_size);
    cfg.mergeRegionArea = (int)rcSqr(c->region_merge_size);
    cfg.maxVertsPerPoly = 6;
    cfg.detailSampleDist = c->detail_sample_dist < 0.9f ? 0 : cfg.cs * c->detail_sample_dist;
    cfg.detailSampleMaxError = cfg.ch * c->detail_sample_max_error;

    rcCalcBounds(verts, nverts, cfg.bmin, cfg.bmax);
    rcCalcGridSize(cfg.bmin, cfg.bmax, cfg.cs, &cfg.width, &cfg.height);

    rcHeightfield* solid = rcAllocHeightfield();
    if (!solid || !rcCreateHeightfield(&ctx, *solid, cfg.width, cfg.height, cfg.bmin, cfg.bmax, cfg.cs, cfg.ch))
        return fail(out, "rcCreateHeightfield");

    unsigned char* areas = (unsigned char*)std::malloc((size_t)ntris);
    if (!areas) return fail(out, "alloc areas");
    std::memset(areas, 0, (size_t)ntris);
    rcMarkWalkableTriangles(&ctx, cfg.walkableSlopeAngle, verts, nverts, tris, ntris, areas);
    // Per-triangle role (tri_obstacle reused as a tri-state source):
    //   1 = obstacle  -> RC_NULL_AREA (blocks, never walkable)
    //   2 = drivable model surface (bridge/ramp deck) -> AREA_MODEL_SURFACE (1)
    //   0 = terrain   -> leave rcMarkWalkableTriangles' RC_WALKABLE_AREA (63)
    if (tri_obstacle) {
        for (int i = 0; i < ntris; i++) {
            if (tri_obstacle[i] == 1) areas[i] = RC_NULL_AREA;
            else if (tri_obstacle[i] == 2 && areas[i] != RC_NULL_AREA) areas[i] = 1; // AREA_MODEL_SURFACE
        }
    }
    if (!rcRasterizeTriangles(&ctx, verts, nverts, tris, areas, ntris, *solid, cfg.walkableClimb)) {
        std::free(areas);
        return fail(out, "rcRasterizeTriangles");
    }
    std::free(areas);

    rcFilterLowHangingWalkableObstacles(&ctx, cfg.walkableClimb, *solid);
    rcFilterLedgeSpans(&ctx, cfg.walkableHeight, cfg.walkableClimb, *solid);
    rcFilterWalkableLowHeightSpans(&ctx, cfg.walkableHeight, *solid);

    rcCompactHeightfield* chf = rcAllocCompactHeightfield();
    if (!chf || !rcBuildCompactHeightfield(&ctx, cfg.walkableHeight, cfg.walkableClimb, *solid, *chf))
        return fail(out, "rcBuildCompactHeightfield");
    rcFreeHeightField(solid);

    if (cfg.walkableRadius > 0 && !rcErodeWalkableArea(&ctx, cfg.walkableRadius, *chf))
        return fail(out, "rcErodeWalkableArea");
    if (!rcBuildDistanceField(&ctx, *chf))
        return fail(out, "rcBuildDistanceField");
    if (!rcBuildRegions(&ctx, *chf, 0, cfg.minRegionArea, cfg.mergeRegionArea))
        return fail(out, "rcBuildRegions");

    rcContourSet* cset = rcAllocContourSet();
    if (!cset || !rcBuildContours(&ctx, *chf, cfg.maxSimplificationError, cfg.maxEdgeLen, *cset))
        return fail(out, "rcBuildContours");

    rcPolyMesh* pmesh = rcAllocPolyMesh();
    if (!pmesh || !rcBuildPolyMesh(&ctx, *cset, cfg.maxVertsPerPoly, *pmesh))
        return fail(out, "rcBuildPolyMesh");

    // Detail mesh: triangulate the walkable surface at ~detailSampleDist spacing,
    // following the ground height sampled from the compact heightfield (the convex
    // polys above are flat plane fits). This is the source for the 1-unit triangle
    // navmesh. Needs pmesh + the still-live compact heightfield.
    rcPolyMeshDetail* dmesh = rcAllocPolyMeshDetail();
    if (!dmesh || !rcBuildPolyMeshDetail(&ctx, *pmesh, *chf, cfg.detailSampleDist, cfg.detailSampleMaxError, *dmesh))
        return fail(out, "rcBuildPolyMeshDetail");

    const int nvp = pmesh->nvp;
    out->nvp = nvp;
    out->nverts = pmesh->nverts;
    out->npolys = pmesh->npolys;
    out->verts = (float*)std::malloc(sizeof(float) * 3 * (size_t)pmesh->nverts);
    out->polys = (int*)std::malloc(sizeof(int) * (size_t)nvp * 2 * (size_t)pmesh->npolys);
    out->areas = (unsigned char*)std::malloc((size_t)pmesh->npolys);
    if (!out->verts || !out->polys || !out->areas) return fail(out, "alloc result");

    for (int i = 0; i < pmesh->nverts; i++) {
        const unsigned short* v = &pmesh->verts[i * 3];
        out->verts[i * 3 + 0] = pmesh->bmin[0] + (float)v[0] * cfg.cs;
        out->verts[i * 3 + 1] = pmesh->bmin[1] + (float)(v[1] + 1) * cfg.ch;
        out->verts[i * 3 + 2] = pmesh->bmin[2] + (float)v[2] * cfg.cs;
    }
    const int total = pmesh->npolys * nvp * 2;
    for (int i = 0; i < total; i++)
        out->polys[i] = (int)pmesh->polys[i]; // 65535 = unused vertex / border edge
    std::memcpy(out->areas, pmesh->areas, (size_t)pmesh->npolys);

    // Export the detail triangulation as a flat global-indexed triangle soup.
    // Per-submesh tri vertex indices are LOCAL (relative to the submesh base
    // vertex); add the base to index into the shared dverts array. ALSO export the
    // per-submesh (= per convex poly) tri range so the runtime/viewer can map each
    // detail triangle back to its parent poly (-> that poly's area, and dense-Y
    // height queries restricted to the right poly). rcBuildPolyMeshDetail emits one
    // submesh per source polygon, in poly order, so dmeshes is aligned with polys.
    out->ndverts = dmesh->nverts;
    out->ndtris = dmesh->ntris;
    out->ndmeshes = dmesh->nmeshes;
    out->dverts = (float*)std::malloc(sizeof(float) * 3 * (size_t)dmesh->nverts);
    out->dtris = (int*)std::malloc(sizeof(int) * 3 * (size_t)dmesh->ntris);
    out->dmeshes = (int*)std::malloc(sizeof(int) * 2 * (size_t)dmesh->nmeshes);
    if (!out->dverts || !out->dtris || !out->dmeshes) return fail(out, "alloc detail");
    if (dmesh->nverts > 0)
        std::memcpy(out->dverts, dmesh->verts, sizeof(float) * 3 * (size_t)dmesh->nverts);
    int dti = 0;
    for (int m = 0; m < dmesh->nmeshes; m++) {
        const unsigned int bverts = dmesh->meshes[m * 4 + 0];
        const unsigned int btris  = dmesh->meshes[m * 4 + 2];
        const unsigned int mtris  = dmesh->meshes[m * 4 + 3];
        out->dmeshes[m * 2 + 0] = dti / 3; // first global triangle index for this poly
        out->dmeshes[m * 2 + 1] = (int)mtris;
        for (unsigned int j = 0; j < mtris; j++) {
            const unsigned char* t = &dmesh->tris[(btris + j) * 4];
            out->dtris[dti++] = (int)(bverts + t[0]);
            out->dtris[dti++] = (int)(bverts + t[1]);
            out->dtris[dti++] = (int)(bverts + t[2]);
        }
    }

    out->ok = 1;
    rcFreeContourSet(cset);
    rcFreeCompactHeightfield(chf);
    rcFreePolyMesh(pmesh);
    rcFreePolyMeshDetail(dmesh);
    return out;
}

extern "C" void recast_free(RcResultC* r) {
    if (!r) return;
    std::free(r->verts);
    std::free(r->polys);
    std::free(r->areas);
    std::free(r->dverts);
    std::free(r->dtris);
    std::free(r->dmeshes);
    std::free(r);
}
