//! `samap` — inspect GTA SA map geometry from a `VER2` IMG archive.
//!
//! Usage:
//!   samap list <archive.img>                 list every entry (name + size)
//!   samap col  <archive.img> [name-substr]   parse .col entries; summarise models + triangles
//!   samap obj  <archive.img> <entry> <out.obj>   dump one entry's collision mesh to Wavefront OBJ
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
        _ => {
            eprintln!(
                "usage:\n  samap list <archive.img>\n  samap col <archive.img> [name-substr]\n  \
                 samap obj <archive.img> <entry> <out.obj>\n  \
                 samap world <sampcol.img> <ipl-dir> [out.obj]"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
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
