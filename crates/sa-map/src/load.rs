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

/// Files of one kind (`IDE` / `IPL`) listed in the game's `.dat` loaders (`gta.dat` +
/// `default.dat` next to it), resolved against the game root (the data dir's parent). This is the
/// game's OWN active-file list: the data dir also carries .ide/.ipl for INACTIVE seasonal/event
/// maps (venator, new-year, …) that reuse model ids of active maps — blindly loading every file
/// lets an inactive map hijack ids (e.g. id 12195: `saw_conveyor` in map_props.ide vs
/// `venator_ray_big` in venator.ide — the sawmill conveyors rendered as 158 m event-light rays).
fn dat_listed_files(data_dir: &Path, kind: &str) -> Vec<PathBuf> {
    let Some(root) = data_dir.parent() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for dat in ["gta.dat", "default.dat"] {
        let Ok(text) = std::fs::read_to_string(data_dir.join(dat)) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some(rest) = line
                .strip_prefix(kind)
                .or_else(|| line.strip_prefix(&kind.to_ascii_lowercase()))
            else {
                continue;
            };
            let rel = rest.trim().replace('\\', "/");
            if !rel.is_empty() {
                out.push(root.join(rel));
            }
        }
    }
    out
}

/// Sibling of `data_dir` (both live directly under the Arizona install root) carrying SA:MP's own
/// extended-object IDE (`SAMP.ide` + `CUSTOM.ide`): ids roughly 18500-19900+ reserved by the SA:MP team
/// for objects not in any vanilla map .ide — Arizona streams its custom job-zone buildings (the sawmill's
/// walls, concrete slabs, …) as `CreateObject`s using exactly these ids, so skipping this directory left
/// every one of them unresolved and invisible in `place_objects`.
const EXTRA_IDE_DIR: &str = "SAMP";

/// Build `id → model name` from every IDE file under `data_dir`, plus the sibling [`EXTRA_IDE_DIR`].
pub fn load_ide_map<P: AsRef<Path>>(data_dir: P) -> HashMap<i32, String> {
    let mut defs = Vec::new();
    // The game's active list when available (see [`dat_listed_files`]); every .ide on disk as a
    // fallback for folders without a gta.dat.
    let mut ide_files = dat_listed_files(data_dir.as_ref(), "IDE");
    if ide_files.is_empty() {
        ide_files = find_ext(&data_dir, "ide");
    }
    for path in ide_files {
        if let Ok(text) = std::fs::read_to_string(&path) {
            defs.extend(ide::parse(&text));
        }
    }
    if let Some(parent) = data_dir.as_ref().parent() {
        for path in find_ext(parent.join(EXTRA_IDE_DIR), "ide") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                defs.extend(ide::parse(&text));
            }
        }
    }
    ide::build_map(defs)
}

/// Parse the `inst` section of every text `.ipl` under `data_dir`.
pub fn load_text_instances<P: AsRef<Path>>(data_dir: P) -> Vec<Instance> {
    let mut out = Vec::new();
    let mut ipl_files = dat_listed_files(data_dir.as_ref(), "IPL");
    if ipl_files.is_empty() {
        ipl_files = find_ext(&data_dir, "ipl");
    }
    for path in ipl_files {
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

/// Parse Arizona's client-side streamer placement database (`data\maps\streamer_exteriors.bin`).
///
/// This file is why chunks of Arizona's custom map never appear on the RakNet wire: the client
/// creates these objects LOCALLY (the sawmill's `barn_sawmill`/`saw_mill` buildings, etc.), so a
/// pcap of a full session contains the ground and props as `CreateObject` RPCs but no buildings.
/// The container has several sections (a custom-model name table, then record arrays) and is not
/// fully reversed; records are recovered with a sliding plausibility scan instead. One record is
/// 42 bytes: `[i32 modelId][f32 x,y,z][f32 quat x,y,z,w][i32 world][f32 streamDist][u16 tail]` —
/// a candidate offset is accepted (and the scan jumps a whole record) only when the id, coordinates,
/// quaternion norm, world and stream distance are all simultaneously plausible, which makes false
/// positives vanishingly unlikely; on rejection the scan slides one byte.
///
/// Rotation convention: the bin stores a **local→world** quaternion — the OPPOSITE of the IPL
/// convention (world→local) that [`Instance::to_world`] conjugates. Verified on the white-bridge
/// row near the sawmill (`cxrf_whitebrig` ×3 along a SW-NE diagonal): applying the IPL conjugate
/// yawed every span ~90° across the roadway, shattering the bridge; applying the quat directly
/// aligns them. We negate the vector part here so the conjugate inside `to_world` cancels out.
pub fn load_streamer_instances<P: AsRef<Path>>(
    path: P,
    ide_map: &HashMap<i32, String>,
) -> Vec<Instance> {
    let Ok(data) = std::fs::read(&path) else {
        return Vec::new();
    };
    let f32_at = |o: usize| f32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let i32_at = |o: usize| i32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let mut out = Vec::new();
    // The file stores overlapping/duplicated record runs (and the sliding scan can re-find them),
    // which places identical instances on top of each other — coplanar triangles that z-fight as
    // shimmering stripes in the viewer. Dedup on (model, quantized position).
    let mut seen: std::collections::HashSet<(i32, i64, i64, i64)> =
        std::collections::HashSet::new();
    let mut o = 0usize;
    while o + 42 <= data.len() {
        let id = i32_at(o);
        if !(300..=30000).contains(&id) {
            o += 1;
            continue;
        }
        let (x, y, z) = (f32_at(o + 4), f32_at(o + 8), f32_at(o + 12));
        let (qx, qy, qz, qw) = (
            f32_at(o + 16),
            f32_at(o + 20),
            f32_at(o + 24),
            f32_at(o + 28),
        );
        let world = i32_at(o + 32);
        let dist = f32_at(o + 36);
        let plausible = x.abs() < 20_000.0
            && y.abs() < 20_000.0
            && (-1_000.0..4_000.0).contains(&z)
            && ((qx * qx + qy * qy + qz * qz + qw * qw).sqrt() - 1.0).abs() < 0.02
            && (world == -1 || (0..=100).contains(&world))
            && dist > 0.0
            && dist <= 4_000.0;
        if !plausible {
            o += 1;
            continue;
        }
        if !seen.insert((id, (x * 10.0) as i64, (y * 10.0) as i64, (z * 10.0) as i64)) {
            o += 42;
            continue; // duplicate placement (see `seen` above)
        }
        let model_name = ide_map.get(&id).cloned().unwrap_or_default();
        out.push(Instance {
            model_id: id,
            model_name,
            interior: 0,
            position: crate::Vec3::new(x, y, z),
            // Negated vector part: cancels `to_world`'s IPL-convention conjugate (see above).
            rotation: [-qx, -qy, -qz, qw],
            lod: -1,
        });
        o += 42;
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

/// SA:MP's own collision archive, sibling of `data_dir`'s parent in the [`EXTRA_IDE_DIR`] folder —
/// carries collision for the whole SAMP.ide extended-object range (walls, concrete slabs, …). Those ids
/// are SA:MP's own additions, not part of the base game map, so neither `gta3.img` nor any
/// [`EXTRA_COLLISION_IMGS`] entry ships their collision — without this, a resolved-by-name streamed
/// object (see [`load_ide_map`]) still placed zero triangles because `world::place_objects`'s
/// by-name collision lookup came up empty.
const SAMP_COLLISION_IMG: &str = "SAMPCOL.img";

/// Assemble the world's collision models + placing instances from a game folder:
/// - collision from the primary IMG (`gta3.img`) plus any [`EXTRA_COLLISION_IMGS`] beside it, plus
///   SA:MP's own [`SAMP_COLLISION_IMG`],
/// - binary IPL instances from the primary IMG (named via the IDE map),
/// - text IPL instances from the data dir.
///
/// Later collision (the extra IMGs) overrides earlier by name in [`world::build`].
pub fn assemble_world<P: AsRef<Path>, Q: AsRef<Path>>(
    primary_img: P,
    data_dir: Q,
) -> Result<(Vec<ColModel>, Vec<Instance>)> {
    assemble_world_opts(primary_img, data_dir, true)
}

/// [`assemble_world`] with the DFF render-mesh visual upgrade selectable.
/// `visual_upgrade = true` swaps sparse proxy collision (tree-trunk cones, empty
/// stubs) for the dense render mesh — right for VIEWING, wrong for a navmesh:
/// in-game a tree canopy has NO collision, so feeding its render mesh to Recast
/// slices walkable clearance under every tree. Navmesh callers pass `false` to
/// get the world as the game physically collides with it.
pub fn assemble_world_opts<P: AsRef<Path>, Q: AsRef<Path>>(
    primary_img: P,
    data_dir: Q,
    visual_upgrade: bool,
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
    if let Some(parent) = data_dir.as_ref().parent() {
        let path = parent.join(EXTRA_IDE_DIR).join(SAMP_COLLISION_IMG);
        if let Ok(extra) = ImgArchive::open(&path) {
            models.extend(load_collision(&extra));
        }
    }

    // Render-mesh fallback/upgrade for Arizona custom models. Two cases, both real at the sawmill:
    // - collision missing or an intentionally EMPTY stub (the `saw_blade` delivery conveyor, the
    //   `obj91_*` props) — without the DFF's visual mesh those placements are invisible holes;
    // - collision that is only a sparse PROXY of the visual (tree-trunk cones, box stand-ins:
    //   `fir_dark_*` firs, `vbg_fir_copse` giant clusters) — those render as bare poles/cylinders.
    // In both cases swap in the DFF render mesh when it is much denser than the collision.
    // Restricted to the custom archives — vanilla collision tracks its visuals well enough, and
    // parsing every vanilla DFF would be expensive.
    if visual_upgrade {
        let mut col_density: HashMap<String, usize> = HashMap::new();
        for m in &models {
            let d = m.faces.len() + m.boxes.len() * 12;
            let e = col_density.entry(m.name.to_ascii_lowercase()).or_insert(0);
            *e = (*e).max(d);
        }
        // Vegetation by naming convention: for these, the render mesh ALWAYS beats the collision —
        // veg collision is a trunk cone / canopy cylinder however dense it is (the giant
        // `vbg_fir_copse` clusters carry a 238-face solid cylinder while their real look is a few
        // hundred billboard planes), so the density-ratio guard below is skipped for them.
        const VEG_NAMES: [&str; 10] = [
            "veg", "tree", "pine", "fir", "redwood", "bush", "palm", "log", "cedar", "plant",
        ];
        let mut upgrade_archive = |archive: &ImgArchive, veg_only: bool| {
            for entry in archive.entries() {
                let lower = entry.name.to_ascii_lowercase();
                let Some(stem) = lower.strip_suffix(".dff") else {
                    continue;
                };
                let is_veg = VEG_NAMES.iter().any(|n| stem.contains(n));
                if veg_only && !is_veg {
                    continue;
                }
                // Never give a LOD proxy a render mesh: its instance sits at the same spot as the real
                // model, so both would draw and z-fight as doubled geometry.
                if stem.starts_with("lod") {
                    continue;
                }
                let blob = archive.read(entry);
                // Instances reference the COLLISION name, which for Arizona replacement DFFs differs
                // from the file stem (`vbg_fir_copse_n.dff` embeds a COL named `vbg_fir_copse`, and
                // the placed instances say `vbg_fir_copse`). Register the render mesh under the
                // embedded COL name when there is one, else the stem — otherwise the swap lands on a
                // name nothing references and changes nothing.
                let col_name = find_col_chunk(blob)
                    .and_then(|off| col::parse_archive(&blob[off..]).ok())
                    .and_then(|ms| ms.first().map(|m| m.name.to_ascii_lowercase()))
                    .filter(|n| !n.is_empty());
                let name = col_name.as_deref().unwrap_or(stem);
                let density = col_density.get(name).copied().unwrap_or(0);
                // A dense-enough collision is representative — keep it (real buildings top out at
                // a few hundred collision faces; only proxies fall well short of the render mesh).
                if density >= 300 {
                    continue;
                }
                let Some(mesh) = crate::dff::parse_render_mesh(blob) else {
                    continue;
                };
                if density > 0 && !is_veg && mesh.faces.len() < density * 4 {
                    continue; // collision is a fair stand-in for a mesh this size — keep it
                }
                col_density.insert(name.to_string(), usize::MAX); // later archives must not re-swap
                models.push(render_mesh_to_col(name, mesh));
            }
        };
        if let Some(dir) = primary_img.as_ref().parent() {
            for name in ["map_object.img", "gamemods.img", "trees.img"] {
                if let Ok(archive) = ImgArchive::open(dir.join(name)) {
                    upgrade_archive(&archive, false);
                }
            }
        }
        // The vanilla archive too, but ONLY for vegetation (poles are trunk-only collision
        // there as well) — swapping every vanilla building would multiply the world mesh
        // several times over.
        upgrade_archive(&primary, true);
    }

    let ide_map = load_ide_map(&data_dir);
    let mut instances = load_binary_instances(&primary, &ide_map);
    instances.extend(load_text_instances(&data_dir));
    // Arizona's client-side streamer placements — custom-map objects (the sawmill buildings, …)
    // that the server never streams over RakNet because the client creates them locally.
    instances.extend(load_streamer_instances(
        data_dir
            .as_ref()
            .join("maps")
            .join("streamer_exteriors.bin"),
        &ide_map,
    ));
    Ok((models, instances))
}

/// Wrap a DFF render mesh as a pseudo-collision model so the placement layer can use it unchanged.
fn render_mesh_to_col(name: &str, mesh: crate::dff::RenderMesh) -> ColModel {
    let mut min = crate::Vec3::new(f32::MAX, f32::MAX, f32::MAX);
    let mut max = crate::Vec3::new(f32::MIN, f32::MIN, f32::MIN);
    for v in &mesh.vertices {
        min = crate::Vec3::new(min.x.min(v.x), min.y.min(v.y), min.z.min(v.z));
        max = crate::Vec3::new(max.x.max(v.x), max.y.max(v.y), max.z.max(v.z));
    }
    ColModel {
        name: name.to_string(),
        model_id: 0,
        version: 0, // marks a render-mesh fallback, not a real COL
        bound_min: min,
        bound_max: max,
        bound_center: crate::Vec3::new(
            (min.x + max.x) * 0.5,
            (min.y + max.y) * 0.5,
            (min.z + max.z) * 0.5,
        ),
        bound_radius: ((max.x - min.x).powi(2) + (max.y - min.y).powi(2) + (max.z - min.z).powi(2))
            .sqrt()
            * 0.5,
        spheres: Vec::new(),
        boxes: Vec::new(),
        vertices: mesh.vertices,
        faces: mesh
            .faces
            .into_iter()
            .map(|[a, b, c]| crate::col::ColTriangle {
                a,
                b,
                c,
                material: 0,
            })
            .collect(),
    }
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
