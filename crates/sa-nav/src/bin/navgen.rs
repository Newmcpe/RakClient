//! `navgen` — build an on-foot navmesh for a SA-world region and write it as a `.nav` file; see docs/memory/sa-nav/navgen.md#module-overview

use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: navgen <gta3.img> <data-dir> <out.nav> [objects.csv] [cx cy half]");
        std::process::exit(2);
    }
    let (img, data_dir, out_path) = (&args[0], &args[1], &args[2]);
    let csv = args.get(3).filter(|s| s.ends_with(".csv"));
    let tail: Vec<f32> = args
        .iter()
        .skip(if csv.is_some() { 4 } else { 3 })
        .filter_map(|s| s.parse().ok())
        .collect();
    let (cx, cy, half) = match tail.as_slice() {
        [a, b, c] => (*a, *b, *c),
        _ => (-520.0, -190.0, 400.0), // default: the sawmill region
    };

    eprintln!("assembling world (collision-only: no visual render-mesh upgrade)…");
    // visual_upgrade = false: the navmesh must see the world the GAME collides
    // with — trunk cones instead of canopies (a canopy has no in-game collision,
    // but its render mesh would slice walkable clearance under every tree).
    let (models, instances) = match sa_map::load::assemble_world_opts(img, data_dir, false) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("world load failed: {e}");
            std::process::exit(1);
        }
    };
    let mut mesh = sa_map::world::build(&models, &instances, Some(0));

    // Streamed-objects overlay (server CreateObjects mined from a pcap) — part of
    // the world the bot collides with, so part of the navmesh input.
    if let Some(csv) = csv {
        let ide_map = sa_map::load::load_ide_map(data_dir);
        let objects = load_objects_csv(csv);
        let overlay = sa_map::world::place_objects(&models, &ide_map, &objects);
        let base = mesh.positions.len() as u32;
        mesh.positions.extend_from_slice(&overlay.positions);
        mesh.indices
            .extend(overlay.indices.iter().map(|i| i + base));
    }

    // Cull to the region, drop unreferenced verts, convert SA -> Y-up.
    let (verts, tris) = cull_region(&mesh, cx, cy, half);
    eprintln!(
        "region [{:.0}±{half:.0}, {:.0}±{half:.0}]: {} tris ({} verts) of {} total",
        cx,
        cy,
        tris.len(),
        verts.len(),
        mesh.indices.len() / 3,
    );
    if tris.is_empty() {
        eprintln!("no geometry in region");
        std::process::exit(1);
    }

    let mut cfg = sa_nav::onfoot_config();
    // Bake tunables overridable from the environment for experiments (obstacle carving
    // vs terrain fragmentation) without recompiling: e.g. NAV_CLIMB=0.4 carves a low
    // коряга/stump the default 0.9 m climb steps over.
    let envf = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
    if let Some(v) = envf("NAV_CLIMB") {
        cfg.agent_max_climb = v;
    }
    if let Some(v) = envf("NAV_RADIUS") {
        cfg.agent_radius = v;
    }
    if let Some(v) = envf("NAV_CELL") {
        cfg.cell_size = v;
    }
    if let Some(v) = envf("NAV_SLOPE") {
        cfg.walkable_slope_angle = v;
    }
    eprintln!(
        "building navmesh (cell {} m, agent r={} h={} climb={} slope={}°, tiled 64 m)…",
        cfg.cell_size,
        cfg.agent_radius,
        cfg.agent_height,
        cfg.agent_max_climb,
        cfg.walkable_slope_angle,
    );
    let t0 = std::time::Instant::now();
    // SA_NAV_SOLO=1 forces the single-tile pipeline — the A/B knob for isolating
    // cross-tile seam-linking problems from genuine geometry fragmentation.
    let solo = std::env::var("SA_NAV_SOLO").is_ok_and(|v| v == "1");
    let built = if solo {
        navmesh_recast::build_rerecast(&verts, &tris, &[], &[], &cfg)
    } else {
        navmesh_recast::build_rerecast_tiled(&verts, &tris, &[], &[], &cfg, 64.0)
    };
    let mut nav = match built {
        Ok(n) => n,
        Err(e) => {
            eprintln!("navmesh build failed: {e}");
            std::process::exit(1);
        }
    };
    let built = t0.elapsed();
    let (kept, dropped, comps) = nav.retain_largest_component();
    eprintln!(
        "built in {:.1?}: {} polys kept, {} dropped ({} unreachable components: roofs/pockets), {} detail tris",
        built,
        kept,
        dropped,
        comps,
        nav.detail_tris.len(),
    );

    let sa = sa_nav::NavMesh::from_recast(&nav);
    let mut f = match std::fs::File::create(out_path) {
        Ok(f) => std::io::BufWriter::new(f),
        Err(e) => {
            eprintln!("cannot create {out_path}: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = sa.save(&mut f) {
        eprintln!("write failed: {e}");
        std::process::exit(1);
    }
    eprintln!("wrote {out_path}");
}

/// Read a streamed-objects CSV (`model_id,x,y,z,rx,ry,rz`).
fn load_objects_csv(path: &str) -> Vec<(i32, sa_map::Vec3, sa_map::Vec3)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("could not read objects csv {path}");
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 {
            continue;
        }
        let p: Vec<f32> = f[1..7]
            .iter()
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if let (Ok(id), 6) = (f[0].trim().parse::<i32>(), p.len()) {
            out.push((
                id,
                sa_map::Vec3::new(p[0], p[1], p[2]),
                sa_map::Vec3::new(p[3], p[4], p[5]),
            ));
        }
    }
    out
}

/// Keep triangles whose SA-xy bbox intersects the region square, compact verts, convert to recast Y-up, and normalise winding up; see docs/memory/sa-nav/navgen.md#cull-region
fn cull_region(mesh: &sa_map::Mesh, cx: f32, cy: f32, half: f32) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let (x0, x1, y0, y1) = (cx - half, cx + half, cy - half, cy + half);
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for t in mesh.indices.chunks_exact(3) {
        let p: Vec<[f32; 3]> = t.iter().map(|&i| mesh.positions[i as usize]).collect();
        if p.iter().all(|v| v[0] < x0)
            || p.iter().all(|v| v[0] > x1)
            || p.iter().all(|v| v[1] < y0)
            || p.iter().all(|v| v[1] > y1)
        {
            continue;
        }
        let mut lt = [0u32; 3];
        for (k, &gi) in t.iter().enumerate() {
            lt[k] = *remap.entry(gi).or_insert_with(|| {
                let idx = verts.len() as u32;
                verts.push(sa_nav::sa_to_recast(mesh.positions[gi as usize]));
                idx
            });
        }
        // Orient up in recast space: flip if the normal's Y is negative.
        let (a, b, c) = (
            verts[lt[0] as usize],
            verts[lt[1] as usize],
            verts[lt[2] as usize],
        );
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let ny = u[2] * v[0] - u[0] * v[2];
        if ny < 0.0 {
            lt.swap(1, 2);
        }
        tris.push(lt);
    }
    (verts, tris)
}
