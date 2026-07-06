//! `samap` — inspect GTA SA map geometry from a `VER2` IMG archive.
//!
//! Usage:
//!   samap list <archive.img>                 list every entry (name + size)
//!   samap col  <archive.img> [name-substr]   parse .col entries; summarise models + triangles
//!   samap obj  <archive.img> <entry> <out.obj>   dump one entry's collision mesh to Wavefront OBJ
//!   samap resolve <data-dir> <model-id>...   id -> name via load_ide_map (incl. sibling SAMP/*.ide)
//!
//! The OBJ dump is the visual sanity check: open it in any 3D viewer and confirm the collision reads
//! as real geometry (a building, terrain patch, etc.) before we build placement + raycasting on top.

use sa_map::{col, ImgArchive};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("list") => list(&args),
        Some("col") => col_summary(&args),
        Some("obj") => dump_obj(&args),
        Some("world") => world_stats(&args),
        Some("probe") => probe(&args),
        Some("hex") => hex_entry(&args),
        Some("find") => find_model(&args),
        Some("resolve") => resolve_ids(&args),
        Some("cover") => cover(&args),
        Some("extract") => extract(&args),
        Some("topdown") => topdown(&args),
        Some("whoat") => whoat(&args),
        Some("scene") => bake_scene(&args),
        _ => {
            eprintln!(
                "usage:\n  samap list <archive.img>\n  samap col <archive.img> [name-substr]\n  \
                 samap obj <archive.img> <entry> <out.obj>\n  \
                 samap world <sampcol.img> <ipl-dir> [out.obj]\n  \
                 samap resolve <data-dir> <model-id>..."
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Which instances (with their models' world bboxes) cover point (x, y)? Attribution for "what is
/// this thing I see at these coordinates" — the flattened world mesh can't answer that.
/// Usage: samap whoat <gta3.img> <data-dir> <x> <y>
fn whoat(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let img = args.get(1).ok_or("missing <gta3.img>")?;
    let data_dir = args.get(2).ok_or("missing <data-dir>")?;
    let x: f32 = args.get(3).ok_or("missing <x>")?.parse()?;
    let y: f32 = args.get(4).ok_or("missing <y>")?.parse()?;
    let (models, instances) = sa_map::load::assemble_world(img, data_dir)?;
    let by_name: std::collections::HashMap<String, &sa_map::ColModel> = models
        .iter()
        .map(|m| (m.name.to_ascii_lowercase(), m))
        .collect();
    for inst in instances.iter().filter(|i| i.interior == 0) {
        // Un-placeable instances have no world bbox; report them by raw distance instead so the
        // missing pieces of a broken structure show up next to the placed ones.
        let near = (inst.position.x - x).hypot(inst.position.y - y) < 80.0;
        let missing = |why: &str| {
            println!(
                "{:<24} id={:<6} pos={:.1},{:.1},{:.1} {}",
                inst.model_name,
                inst.model_id,
                inst.position.x,
                inst.position.y,
                inst.position.z,
                why,
            );
        };
        let Some(m) = by_name.get(&inst.model_name.to_ascii_lowercase()) else {
            if near {
                missing("<- MODEL NOT FOUND");
            }
            continue;
        };
        if m.faces.is_empty() && m.boxes.is_empty() {
            if near {
                missing("<- EMPTY COLLISION");
            }
            continue;
        }
        // World-space bbox from the model's local bounds via 8 transformed corners.
        let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
        for i in 0..8u8 {
            let c = sa_map::Vec3::new(
                if i & 1 == 0 {
                    m.bound_min.x
                } else {
                    m.bound_max.x
                },
                if i & 2 == 0 {
                    m.bound_min.y
                } else {
                    m.bound_max.y
                },
                if i & 4 == 0 {
                    m.bound_min.z
                } else {
                    m.bound_max.z
                },
            );
            let w = inst.to_world(c);
            for (k, v) in [w.x, w.y, w.z].iter().enumerate() {
                min[k] = min[k].min(*v);
                max[k] = max[k].max(*v);
            }
        }
        if x >= min[0] && x <= max[0] && y >= min[1] && y <= max[1] {
            println!(
                "{:<24} id={:<6} pos={:.1},{:.1},{:.1} bboxZ=[{:.1}..{:.1}] tris={} boxes={} v{}",
                inst.model_name,
                inst.model_id,
                inst.position.x,
                inst.position.y,
                inst.position.z,
                min[2],
                max[2],
                m.faces.len(),
                m.boxes.len(),
                m.version,
            );
        }
    }
    Ok(())
}

/// Offline top-down raster of a world region: base collision height-shaded gray, streamed-CSV overlay
/// in orange — a GUI-free way to verify object placement against an in-game screenshot.
/// Usage: samap topdown <gta3.img> <data-dir> <objects.csv> <out.ppm> [cx cy half]
fn topdown(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let img = args.get(1).ok_or("missing <gta3.img>")?;
    let data_dir = args.get(2).ok_or("missing <data-dir>")?;
    let csv = args.get(3).ok_or("missing <objects.csv>")?;
    let out = args.get(4).ok_or("missing <out.ppm>")?;
    let cx: f32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(-520.0);
    let cy: f32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(-190.0);
    let half: f32 = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(130.0);

    let (models, instances) = sa_map::load::assemble_world(img, data_dir)?;
    let base = sa_map::world::build(&models, &instances, Some(0));
    let ide_map = sa_map::load::load_ide_map(data_dir);
    let text = std::fs::read_to_string(csv)?;
    let mut objects = Vec::new();
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
            objects.push((
                id,
                sa_map::Vec3::new(p[0], p[1], p[2]),
                sa_map::Vec3::new(p[3], p[4], p[5]),
            ));
        }
    }
    let overlay = sa_map::world::place_objects(&models, &ide_map, &objects);

    const W: usize = 900;
    const H: usize = 900;
    let scale = W as f32 / (half * 2.0); // px per metre
                                         // Per-pixel: best z so far and whether the winner is overlay.
    let mut zbuf = vec![f32::NEG_INFINITY; W * H];
    let mut kind = vec![0u8; W * H]; // 0 = nothing, 1 = base, 2 = overlay
    let mut raster = |mesh: &sa_map::Mesh, k: u8| {
        for t in mesh.indices.chunks_exact(3) {
            let v: Vec<[f32; 3]> = t.iter().map(|&i| mesh.positions[i as usize]).collect();
            // Quick reject: outside the window.
            if v.iter().all(|p| p[0] < cx - half)
                || v.iter().all(|p| p[0] > cx + half)
                || v.iter().all(|p| p[1] < cy - half)
                || v.iter().all(|p| p[1] > cy + half)
            {
                continue;
            }
            // Rasterise by bounding box + barycentric test. +y is up in world; flip to image rows.
            let px = |p: &[f32; 3]| ((p[0] - (cx - half)) * scale, ((cy + half) - p[1]) * scale);
            let (x0, y0) = px(&v[0]);
            let (x1, y1) = px(&v[1]);
            let (x2, y2) = px(&v[2]);
            let minx = x0.min(x1).min(x2).floor().max(0.0) as usize;
            let maxx = (x0.max(x1).max(x2).ceil() as usize).min(W - 1);
            let miny = y0.min(y1).min(y2).floor().max(0.0) as usize;
            let maxy = (y0.max(y1).max(y2).ceil() as usize).min(H - 1);
            let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
            if area.abs() < 1e-6 {
                continue;
            }
            for py in miny..=maxy {
                for pxi in minx..=maxx {
                    let (fx, fy) = (pxi as f32 + 0.5, py as f32 + 0.5);
                    let w0 = ((x1 - fx) * (y2 - fy) - (x2 - fx) * (y1 - fy)) / area;
                    let w1 = ((x2 - fx) * (y0 - fy) - (x0 - fx) * (y2 - fy)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < -0.001 || w1 < -0.001 || w2 < -0.001 {
                        continue;
                    }
                    let z = w0 * v[0][2] + w1 * v[1][2] + w2 * v[2][2];
                    let idx = py * W + pxi;
                    if z > zbuf[idx] {
                        zbuf[idx] = z;
                        kind[idx] = k;
                    }
                }
            }
        }
    };
    raster(&base, 1);
    raster(&overlay, 2);

    let mut ppm = format!("P6\n{W} {H}\n255\n").into_bytes();
    for i in 0..W * H {
        let (r, g, b) = match kind[i] {
            2 => (242u8, 115u8, 13u8), // overlay orange
            1 => {
                // base: height-shaded gray-green
                let z = zbuf[i].clamp(0.0, 120.0) / 120.0;
                let v = (60.0 + z * 170.0) as u8;
                (v, v, (v as f32 * 0.9) as u8)
            }
            _ => (25u8, 60u8, 110u8), // nothing: "hole" blue
        };
        ppm.extend_from_slice(&[r, g, b]);
    }
    std::fs::write(out, ppm)?;
    println!(
        "wrote {out}: {W}x{H} px, window x[{}..{}] y[{}..{}], base {} tris, overlay {} tris",
        cx - half,
        cx + half,
        cy - half,
        cy + half,
        base.triangle_count(),
        overlay.triangle_count()
    );
    Ok(())
}

/// Bake a viewer scene: assemble the world once (visual render-mesh upgrade ON,
/// like the viewer) and dump the base collision mesh + streamed-object overlay +
/// hole markers to a `.scene` file, so `sa-viewer` opens instantly instead of
/// re-parsing every IMG/streamer bin/DFF on each launch.
/// Usage: samap scene <gta3.img> <data-dir> <out.scene> [objects.csv]
fn bake_scene(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let img = args.get(1).ok_or("missing <gta3.img>")?;
    let data_dir = args.get(2).ok_or("missing <data-dir>")?;
    let out = args.get(3).ok_or("missing <out.scene>")?;
    let csv = args.get(4);

    eprintln!("assembling world…");
    let (models, instances) = sa_map::load::assemble_world(img, data_dir)?;
    let base = sa_map::world::build(&models, &instances, Some(0));

    // Hole markers: interior-0 instances whose model placed NO collision and is
    // NOT a LOD/no-collision-by-design proxy (the same rule the viewer used).
    let known: std::collections::HashSet<String> =
        models.iter().map(|m| m.name.to_ascii_lowercase()).collect();
    let no_collision_by_design = |n: &str| {
        n.is_empty()
            || n == "dummy"
            || n.starts_with("lod")
            || n.contains("lod")
            || n.ends_with("_l")
            || n.ends_with("_ol")
            || n.ends_with("_ld")
    };
    let mut holes: Vec<[f32; 3]> = Vec::new();
    for i in instances.iter().filter(|i| i.interior == 0) {
        let nm = i.model_name.to_ascii_lowercase();
        if known.contains(&nm) || no_collision_by_design(&nm) {
            continue;
        }
        holes.push([i.position.x, i.position.y, i.position.z]);
    }

    // Streamed-object overlay from a pcap CSV (optional).
    let streamed = if let Some(csv) = csv {
        let ide_map = sa_map::load::load_ide_map(data_dir);
        let text = std::fs::read_to_string(csv)?;
        let mut objects = Vec::new();
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
                objects.push((
                    id,
                    sa_map::Vec3::new(p[0], p[1], p[2]),
                    sa_map::Vec3::new(p[3], p[4], p[5]),
                ));
            }
        }
        sa_map::world::place_objects(&models, &ide_map, &objects)
    } else {
        sa_map::Mesh::default()
    };

    let scene = sa_map::scene::Scene {
        base,
        streamed,
        holes,
    };
    let mut f = std::io::BufWriter::new(std::fs::File::create(out)?);
    scene.save(&mut f)?;
    use std::io::Write;
    f.flush()?;
    eprintln!(
        "wrote {out}: base {} tris, streamed {} tris, {} holes",
        scene.base.triangle_count(),
        scene.streamed.triangle_count(),
        scene.holes.len(),
    );
    Ok(())
}

/// Dump one IMG entry's raw bytes to a file, for offline inspection with external tools.
fn extract(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let archive = open(args)?;
    let name = args.get(2).ok_or("missing <entry>")?;
    let out = args.get(3).ok_or("missing <out-file>")?;
    let blob = archive.get(name).ok_or("entry not found")?;
    std::fs::write(out, blob)?;
    println!("wrote {} bytes to {out}", blob.len());
    Ok(())
}

/// Per-placement collision coverage for a streamed-objects CSV: which model ids resolve to a name AND
/// to real collision geometry, exactly as the viewer's `place_objects` sees them. For every id that
/// resolves to no geometry, scans every sibling `.img` (models dir + `SAMP/`) for a same-named entry so
/// the missing collision's actual home is reported, not guessed.
fn cover(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let img = args.get(1).ok_or("missing <gta3.img>")?;
    let data_dir = args.get(2).ok_or("missing <data-dir>")?;
    let csv = args.get(3).ok_or("missing <objects.csv>")?;

    let (models, _) = sa_map::load::assemble_world(img, data_dir)?;
    let ide_map = sa_map::load::load_ide_map(data_dir);
    let by_name: std::collections::HashMap<String, &sa_map::ColModel> = models
        .iter()
        .map(|m| (m.name.to_ascii_lowercase(), m))
        .collect();

    // Count placements per model id from the CSV.
    let text = std::fs::read_to_string(csv)?;
    let mut counts: std::collections::HashMap<i32, u32> = std::collections::HashMap::new();
    for line in text.lines().skip(1) {
        if let Some(id) = line.split(',').next().and_then(|s| s.trim().parse().ok()) {
            *counts.entry(id).or_default() += 1;
        }
    }

    let mut missing_names: Vec<(i32, String, u32)> = Vec::new();
    let mut ids: Vec<_> = counts.iter().collect();
    ids.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (&id, &n) in ids {
        let Some(name) = ide_map.get(&id) else {
            println!("{n:>3}x  {id:<6} <NO IDE NAME>");
            continue;
        };
        match by_name.get(&name.to_ascii_lowercase()) {
            Some(m) if !m.faces.is_empty() || !m.boxes.is_empty() => {}
            _ => {
                println!("{n:>3}x  {id:<6} {name:<28} NO COLLISION");
                missing_names.push((id, name.clone(), n));
            }
        }
    }
    println!(
        "\n{} of {} distinct ids lack collision; hunting their entries across sibling IMGs…",
        missing_names.len(),
        counts.len()
    );

    // Where do the missing models actually live? Check every .img under models/ and SAMP/.
    let mut img_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(d) = std::path::Path::new(img).parent() {
        img_dirs.push(d.to_path_buf());
    }
    if let Some(p) = std::path::Path::new(data_dir).parent() {
        img_dirs.push(p.join("SAMP"));
    }
    for dir in img_dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("img"))
            {
                continue;
            }
            let Ok(archive) = sa_map::ImgArchive::open(&path) else {
                continue;
            };
            for entry in archive.entries() {
                let stem = entry.name.to_ascii_lowercase();
                let stem = stem.trim_end_matches(".dff").trim_end_matches(".col");
                for (id, name, _) in &missing_names {
                    if stem == name.to_ascii_lowercase() {
                        println!("  {id} {name}: {} in {}", entry.name, path.display());
                    }
                }
            }
        }
    }
    Ok(())
}

/// `id -> name` via [`sa_map::load::load_ide_map`] — a quick way to check whether a server-streamed
/// `CreateObject` model id (e.g. from the `objects` pcap extractor) actually resolves, without needing
/// a GUI viewer run.
fn resolve_ids(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = args.get(1).ok_or("missing <data-dir>")?;
    let ide_map = sa_map::load::load_ide_map(data_dir);
    for id_str in &args[2..] {
        let id: i32 = id_str.parse()?;
        match ide_map.get(&id) {
            Some(name) => println!("{id} -> {name}"),
            None => println!("{id} -> <unresolved>"),
        }
    }
    println!("\n{} total ide entries loaded", ide_map.len());
    Ok(())
}

fn open(args: &[String]) -> Result<ImgArchive, Box<dyn std::error::Error>> {
    let path = args.get(1).ok_or("missing <archive.img>")?;
    Ok(ImgArchive::open(path)?)
}

fn list(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let archive = open(args)?;
    for e in archive.entries() {
        println!("{:<28} {:>8} B", e.name, e.size);
    }
    println!("\n{} entries", archive.entries().len());
    Ok(())
}

fn col_summary(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let archive = open(args)?;
    let filter = args.get(2).map(|s| s.to_ascii_lowercase());

    let (mut entries, mut models, mut tris, mut errors) = (0u64, 0u64, 0u64, 0u64);
    for entry in archive.entries() {
        if let Some(f) = &filter {
            if !entry.name.to_ascii_lowercase().contains(f.as_str()) {
                continue;
            }
        }
        match col::parse_archive(archive.read(entry)) {
            Ok(ms) if !ms.is_empty() => {
                entries += 1;
                models += ms.len() as u64;
                for m in &ms {
                    tris += m.triangles().count() as u64;
                }
                if filter.is_some() {
                    for m in &ms {
                        println!(
                            "{:<24} id={:<6} v{} verts={:<6} tris={}",
                            m.name,
                            m.model_id,
                            m.version,
                            m.vertices.len(),
                            m.triangles().count()
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(_) => errors += 1,
        }
    }
    println!(
        "\ncol entries: {entries}, models: {models}, triangles: {tris}, unparsed entries: {errors}"
    );
    Ok(())
}

fn find_model(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let archive = open(args)?;
    let query = args
        .get(2)
        .ok_or("missing <model-name-substr>")?
        .to_ascii_lowercase();
    // Full collision incl. DFF-embedded COL, so `find` sees custom-model collision too.
    let models = sa_map::load::load_collision(&archive);
    let mut hits = 0;
    for m in &models {
        if m.name.to_ascii_lowercase().contains(&query) {
            println!(
                "{:<28} v{} verts={:<5} faces={:<5} boxes={:<3} spheres={}",
                m.name,
                m.version,
                m.vertices.len(),
                m.faces.len(),
                m.boxes.len(),
                m.spheres.len()
            );
            hits += 1;
        }
    }
    println!(
        "\n{hits} model(s) matching {query:?} ({} total collision models)",
        models.len()
    );
    Ok(())
}

fn hex_entry(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let archive = open(args)?;
    let name = args.get(2).ok_or("missing <entry>")?;
    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(64);
    let blob = archive.get(name).ok_or("entry not found")?;
    for (row, chunk) in blob
        .iter()
        .take(n)
        .collect::<Vec<_>>()
        .chunks(16)
        .enumerate()
    {
        let hexs: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        let asci: String = chunk
            .iter()
            .map(|&&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("{:04X}  {:<48}  {asci}", row * 16, hexs.join(" "));
    }
    Ok(())
}

fn probe(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let archive = open(args)?;
    let count_cc = |b: &[u8], cc: &[u8]| b.windows(4).filter(|w| *w == cc).count();
    let mut worst = 0i64;
    for entry in archive.entries() {
        if !entry.name.to_ascii_lowercase().ends_with(".col") {
            continue;
        }
        let blob = archive.read(entry);
        let raw = count_cc(blob, b"COL2") + count_cc(blob, b"COL3");
        let parsed = col::parse_archive(blob).map(|m| m.len()).unwrap_or(0);
        let gap = raw as i64 - parsed as i64;
        if gap > 0 {
            println!(
                "{:<20} raw COL2/3={raw:<5} parsed={parsed:<5} MISSED={gap}",
                entry.name
            );
            worst += gap;
        }
    }
    println!("\ntotal models missed by early-stop across entries: {worst}");
    Ok(())
}

fn world_stats(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let img = args.get(1).ok_or("missing <gta3.img>")?;
    let data_dir = args.get(2).ok_or("missing <data-dir>")?;

    let (models, instances) = sa_map::load::assemble_world(img, data_dir)?;
    let exterior = instances.iter().filter(|i| i.interior == 0).count();
    let mesh = sa_map::world::build(&models, &instances, Some(0));

    // Coverage: which interior-0 instances found a collision model with a mesh, and which didn't?
    // Missing terrain/ground shows up here as unmatched model names.
    use std::collections::{HashMap, HashSet};
    let ids: HashSet<i32> = models.iter().map(|m| m.model_id as i32).collect();
    let names: HashSet<String> = models.iter().map(|m| m.name.to_ascii_lowercase()).collect();
    let with_mesh: HashSet<String> = models
        .iter()
        .filter(|m| !m.faces.is_empty())
        .map(|m| m.name.to_ascii_lowercase())
        .collect();
    // Model index by name → (has boxes, has spheres) so we can tell box/sphere-only from truly empty.
    let prims: HashMap<String, (bool, bool)> = models
        .iter()
        .map(|m| {
            (
                m.name.to_ascii_lowercase(),
                (!m.boxes.is_empty(), !m.spheres.is_empty()),
            )
        })
        .collect();

    let (mut matched, mut no_mesh, mut unmatched) = (0u64, 0u64, 0u64);
    let (mut miss, mut faceless): (HashMap<String, u64>, HashMap<String, u64>) =
        (HashMap::new(), HashMap::new());
    for i in instances.iter().filter(|i| i.interior == 0) {
        let nm = i.model_name.to_ascii_lowercase();
        if with_mesh.contains(&nm) {
            matched += 1;
        } else if ids.contains(&i.model_id) || names.contains(&nm) {
            no_mesh += 1;
            let tag = match prims.get(&nm) {
                Some((true, _)) => format!("{nm} [box]"),
                Some((_, true)) => format!("{nm} [sphere]"),
                _ => format!("{nm} [EMPTY]"),
            };
            *faceless.entry(tag).or_default() += 1;
        } else {
            unmatched += 1;
            *miss.entry(nm).or_default() += 1;
        }
    }
    let sorted = |m: HashMap<String, u64>| {
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        v
    };
    println!(
        "coverage (interior 0): matched(mesh)={matched} no-mesh-model={no_mesh} UNMATCHED={unmatched}"
    );
    println!("top no-mesh models (found but no triangles):");
    for (n, c) in sorted(faceless).iter().take(15) {
        println!("  {c:>5}  {n}");
    }
    // Split unmatched into LOD proxies (expected: no collision) vs real objects (genuine holes).
    let is_lod = |n: &str| n.starts_with("lod");
    let sorted_miss = sorted(miss);
    let nonlod_count: u64 = sorted_miss
        .iter()
        .filter(|(n, _)| !is_lod(n))
        .map(|(_, c)| c)
        .sum();
    println!("unmatched breakdown: LOD-proxy (no collision, fine) vs REAL objects (holes) = {nonlod_count} real");
    println!("top NON-LOD unmatched (the actual holes):");
    for (n, c) in sorted_miss.iter().filter(|(n, _)| !is_lod(n)).take(20) {
        println!("  {c:>5}  {n}");
    }
    println!();

    // World bounds — a sanity check that placement landed geometry across the real ±3000 map extent.
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &mesh.positions {
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    println!(
        "collision models: {}\ninstances: {} (interior 0: {})\nplaced triangles: {}\nvertices: {}",
        models.len(),
        instances.len(),
        exterior,
        mesh.triangle_count(),
        mesh.positions.len(),
    );
    if !mesh.positions.is_empty() {
        println!(
            "world bounds: x[{:.0}..{:.0}] y[{:.0}..{:.0}] z[{:.0}..{:.0}]",
            min[0], max[0], min[1], max[1], min[2], max[2]
        );
    }

    if let Some(out) = args.get(3) {
        let mut obj = String::from("# placed world collision exported by samap\n");
        for p in &mesh.positions {
            obj.push_str(&format!("v {} {} {}\n", p[0], p[1], p[2]));
        }
        for tri in mesh.indices.chunks_exact(3) {
            obj.push_str(&format!("f {} {} {}\n", tri[0] + 1, tri[1] + 1, tri[2] + 1));
        }
        std::fs::write(out, obj)?;
        println!("wrote {out}");
    }
    Ok(())
}

fn dump_obj(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let archive = open(args)?;
    let name = args.get(2).ok_or("missing <entry>")?;
    let out = args.get(3).ok_or("missing <out.obj>")?;
    let blob = archive
        .get(name)
        .ok_or_else(|| format!("entry {name:?} not found"))?;
    let models = col::parse_archive(blob)?;

    let mut obj = String::new();
    obj.push_str("# collision mesh exported by samap\n");
    let mut base = 0usize; // OBJ vertex indices are 1-based and global across the file
    let mut total_tris = 0usize;
    for m in &models {
        obj.push_str(&format!("o {}_{}\n", m.name, m.model_id));
        for v in &m.vertices {
            obj.push_str(&format!("v {} {} {}\n", v.x, v.y, v.z));
        }
        for f in &m.faces {
            // Skip degenerate/out-of-range faces so the OBJ stays valid.
            let (a, b, c) = (f.a as usize, f.b as usize, f.c as usize);
            if a.max(b).max(c) < m.vertices.len() {
                obj.push_str(&format!(
                    "f {} {} {}\n",
                    base + a + 1,
                    base + b + 1,
                    base + c + 1
                ));
                total_tris += 1;
            }
        }
        base += m.vertices.len();
    }
    std::fs::write(out, obj)?;
    println!(
        "wrote {out}: {} model(s), {} vertices, {} triangles",
        models.len(),
        base,
        total_tris
    );
    Ok(())
}
