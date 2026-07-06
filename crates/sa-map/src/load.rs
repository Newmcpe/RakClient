//! File orchestration: assemble a placed world from a game folder, so the `samap` CLI and the Bevy
//! viewer share one loading path. Parsing lives in the other modules; this reads the folder.
//!
//! The bulk of the SA map is placed by **binary IPLs embedded in `gta3.img`**, keyed by model id;
//! collision models are keyed by name (their embedded id is junk). So the pipeline is:
//! 1. collision `.col` entries from the IMG  → models by name
//! 2. IDE files under the data dir            → id → name
//! 3. binary `.ipl` entries in the IMG        → instances (id) → name via the IDE map
//! 4. text `.ipl` files under the data dir     → instances (already named)
//! 5. place everything against the collision by name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{col, ide, img::ImgArchive, ipl, world, ColModel, Instance, Mesh, Result};

/// Load every collision model from an IMG: standalone `.col` entries, plus the RenderWare COL chunk
/// embedded inside `.dff` model files (how Arizona's custom map ships most of its collision — a COL2/
/// COL3 blob sitting inside the DFF stream, the same format, just wrapped). We locate it by scanning
/// the DFF bytes for the `COL2`/`COL3` FourCC and parsing from there.
pub fn load_collision(archive: &ImgArchive) -> Vec<ColModel> {
    let mut models = Vec::new();
    for entry in archive.entries() {
        let name = entry.name.to_ascii_lowercase();
        let blob = archive.read(entry);
        if name.ends_with(".col") {
            if let Ok(mut ms) = col::parse_archive(blob) {
                models.append(&mut ms);
            }
        } else if name.ends_with(".dff") {
            if let Some(off) = find_col_chunk(blob) {
                if let Ok(mut ms) = col::parse_archive(&blob[off..]) {
                    models.append(&mut ms);
                }
            }
        }
    }
    models
}

/// Offset of the first `COL2`/`COL3` FourCC in a blob (a DFF's embedded collision chunk), or `None`.
fn find_col_chunk(blob: &[u8]) -> Option<usize> {
    blob.windows(4).position(|w| w == b"COL2" || w == b"COL3")
}

/// Recursively collect files with the given (case-insensitive) extension under `dir`.
fn find_ext<P: AsRef<Path>>(dir: P, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.as_ref().to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Build `id → model name` from every IDE file under `data_dir`.
pub fn load_ide_map<P: AsRef<Path>>(data_dir: P) -> HashMap<i32, String> {
    let mut defs = Vec::new();
    for path in find_ext(data_dir, "ide") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            defs.extend(ide::parse(&text));
        }
    }
    ide::build_map(defs)
}

/// Parse the `inst` section of every text `.ipl` under `data_dir`.
pub fn load_text_instances<P: AsRef<Path>>(data_dir: P) -> Vec<Instance> {
    let mut out = Vec::new();
    for path in find_ext(data_dir, "ipl") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.append(&mut ipl::parse(&text));
        }
    }
    out
}

/// Parse every binary (`bnry`) IPL entry inside an IMG, resolving each instance's name from `ide_map`.
pub fn load_binary_instances(
    archive: &ImgArchive,
    ide_map: &HashMap<i32, String>,
) -> Vec<Instance> {
    let mut out = Vec::new();
    for entry in archive.entries() {
        if !entry.name.to_ascii_lowercase().ends_with(".ipl") {
            continue;
        }
        let mut insts = ipl::parse_binary(archive.read(entry));
        for inst in &mut insts {
            if inst.model_name.is_empty() {
                if let Some(name) = ide_map.get(&inst.model_id) {
                    inst.model_name = name.clone();
                }
            }
        }
        out.append(&mut insts);
    }
    out
}

/// Sibling IMGs (next to the primary `gta3.img`) that carry additional collision the main archive
/// lacks — the Arizona custom-map archives its `data\gta.dat` + fastman92 mount alongside vanilla.
/// Collision is a mix of standalone `.col` and DFF-embedded COL (see [`load_collision`]). Their models
/// override same-named ones from the primary.
const EXTRA_COLLISION_IMGS: [&str; 6] = [
    "proper_fixes.img",
    "map_object.img",
    "gamemods.img",
    "custom_int.img",
    "gta_int.img",
    "pubg.img",
];

/// Assemble the world's collision models + placing instances from a game folder:
/// - collision from the primary IMG (`gta3.img`) plus any [`EXTRA_COLLISION_IMGS`] beside it,
/// - binary IPL instances from the primary IMG (named via the IDE map),
/// - text IPL instances from the data dir.
///
/// Later collision (the extra IMGs) overrides earlier by name in [`world::build`].
pub fn assemble_world<P: AsRef<Path>, Q: AsRef<Path>>(
    primary_img: P,
    data_dir: Q,
) -> Result<(Vec<ColModel>, Vec<Instance>)> {
    let primary = ImgArchive::open(&primary_img)?;
    let mut models = load_collision(&primary);
    if let Some(dir) = primary_img.as_ref().parent() {
        for name in EXTRA_COLLISION_IMGS {
            let path = dir.join(name);
            if let Ok(extra) = ImgArchive::open(&path) {
                models.extend(load_collision(&extra));
            }
        }
    }

    let ide_map = load_ide_map(&data_dir);
    let mut instances = load_binary_instances(&primary, &ide_map);
    instances.extend(load_text_instances(&data_dir));
    Ok((models, instances))
}

/// One-shot: primary IMG (+ sibling collision IMGs & binary IPLs) + data dir (IDEs + text IPLs) →
/// merged world-space mesh for `interior` (e.g. `Some(0)` for the outdoor map).
pub fn load_world<P: AsRef<Path>, Q: AsRef<Path>>(
    img_path: P,
    data_dir: Q,
    interior: Option<i32>,
) -> Result<Mesh> {
    let (models, instances) = assemble_world(img_path, data_dir)?;
    Ok(world::build(&models, &instances, interior))
}
