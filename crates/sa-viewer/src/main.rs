//! Bevy flythrough of the GTA SA collision geometry parsed by `sa-map`.
//!
//! Loads collision from an IMG (default: Arizona's `gta3.img`), places every outdoor IPL instance
//! into one world mesh, and lets you fly around to eyeball that the geometry is real and correctly
//! placed. Coordinates are converted from SA (Z-up) to Bevy (Y-up): `(x, y, z) → (x, z, -y)`.
//!
//! Run:  cargo run --release --manifest-path crates/sa-viewer/Cargo.toml [gta3.img] [maps-dir]
//!
//! Controls: hold RIGHT MOUSE to look · WASD move · Space/Ctrl up/down · Shift boost.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::render::mesh::PrimitiveTopology;
use bevy::render::render_asset::RenderAssetUsages;

const DEFAULT_IMG: &str =
    r"C:\Users\Newmcpeishka\AppData\Local\Programs\Arizona Games Launcher\bin\arizona\models\gta3.img";
const DEFAULT_MAPS: &str =
    r"C:\Users\Newmcpeishka\AppData\Local\Programs\Arizona Games Launcher\bin\arizona\data";

/// The parsed world geometry, handed to the setup system to build a Bevy mesh from.
#[derive(Resource)]
struct Geometry(sa_map::Mesh);

/// SA-space positions of instances whose collision didn't place (the real "holes"), for red markers.
#[derive(Resource)]
struct Holes(Vec<[f32; 3]>);

/// Tags the hole-marker mesh so the `H` key can toggle its visibility.
#[derive(Component)]
struct HoleMarkers;

/// Server-streamed `CreateObject` overlay (Arizona's custom map), rendered distinctly over the base.
#[derive(Resource)]
struct StreamedObjects(sa_map::Mesh);

/// Read an `objects` CSV (`model_id,x,y,z,rx,ry,rz`) into placement tuples for `world::place_objects`.
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
        let p: Vec<f32> = f[1..7].iter().filter_map(|s| s.trim().parse().ok()).collect();
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

/// The fly camera's accumulated look angles (radians).
#[derive(Component, Default)]
struct FlyCam {
    yaw: f32,
    pitch: f32,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let img = args.next().unwrap_or_else(|| DEFAULT_IMG.to_string());
    let maps = args.next().unwrap_or_else(|| DEFAULT_MAPS.to_string());

    println!("loading collision from {img}\nplacing instances from {maps}");
    let (models, instances) = match sa_map::load::assemble_world(&img, &maps) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("failed to load world: {e}");
            std::process::exit(1);
        }
    };
    let mesh = sa_map::world::build(&models, &instances, Some(0));

    // Diagnostic markers: interior-0 instances whose model placed NO collision and is NOT a LOD proxy —
    // these are the real "holes". A red marker at each shows exactly where geometry is missing.
    use std::collections::HashSet;
    // A real hole = an instance whose model's collision is absent from ALL loaded files. A model that
    // IS present but has zero primitives (bushes, walk-through props) is no-collision *by design*, not
    // a hole — so we key on "known" (name present in any .col), not "has primitives".
    let known: HashSet<String> = models.iter().map(|m| m.name.to_ascii_lowercase()).collect();
    // A model with no collision by design: LOD proxies (name conventions: starts/contains "lod", or
    // ends in the "_l"/"_ol"/"_ld" LOD suffixes) and "dummy" placeholders. These aren't real holes.
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
    // Optional overlay: server-streamed CreateObject placements (Arizona's custom map) from a CSV
    // produced by `objects <pcap>`. Resolved to collision via the IDE map, placed with euler rotation.
    let streamed = if let Some(csv) = args.next() {
        let objects = load_objects_csv(&csv);
        let ide_map = sa_map::load::load_ide_map(&maps);
        let m = sa_map::world::place_objects(&models, &ide_map, &objects);
        println!(
            "streamed-object overlay: {} placements → {} triangles",
            objects.len(),
            m.triangle_count()
        );
        m
    } else {
        sa_map::Mesh::default()
    };

    println!(
        "loaded {} triangles ({} vertices), {} unplaced-object markers — opening viewer…\ncontrols: RIGHT MOUSE look · WASD move · Space/Ctrl up/down · Shift boost · H toggle markers · P print pos",
        mesh.triangle_count(),
        mesh.positions.len(),
        holes.len(),
    );

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "sa-viewer — GTA SA collision".into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.45, 0.62, 0.82)))
        .insert_resource(AmbientLight {
            color: Color::WHITE,
            brightness: 350.0,
        })
        .insert_resource(Geometry(mesh))
        .insert_resource(Holes(holes))
        .insert_resource(StreamedObjects(streamed))
        .add_systems(Startup, setup)
        .add_systems(Update, (fly, toggle_markers, report_position))
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    geometry: Res<Geometry>,
    holes: Res<Holes>,
    streamed: Res<StreamedObjects>,
) {
    // Expand to a non-indexed mesh (each triangle its own 3 vertices) so flat normals compute cleanly,
    // converting SA (x, y, z / Z-up) into Bevy (x, z, -y / Y-up) as we go.
    let src = &geometry.0;
    let mut positions = Vec::with_capacity(src.indices.len());
    let mut colors = Vec::with_capacity(src.indices.len());
    for &i in &src.indices {
        let p = src.positions[i as usize];
        positions.push([p[0], p[2], -p[1]]);
        colors.push(height_color(p[2])); // p[2] = SA z = ground height
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.compute_flat_normals();

    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::WHITE, // modulated by the per-vertex height colour
            perceptual_roughness: 0.95,
            cull_mode: None, // collision faces are one-sided; show both so nothing vanishes
            ..default()
        })),
    ));

    // Water plane at SA sea level (z=0 → Bevy y=0): the SA ocean/bays have no floor collision, so this
    // makes water read as water instead of sky-blue "holes". Genuine gaps in the land show above it.
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(8000.0, 8000.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgba(0.10, 0.28, 0.42, 1.0),
            perceptual_roughness: 0.3,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, 0.0),
    ));

    // Streamed CreateObject overlay (Arizona custom map): render in orange over the base collision.
    if !streamed.0.indices.is_empty() {
        let src = &streamed.0;
        let mut positions = Vec::with_capacity(src.indices.len());
        for &i in &src.indices {
            let p = src.positions[i as usize];
            positions.push([p[0], p[2], -p[1]]);
        }
        let mut om = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        om.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        om.compute_flat_normals();
        commands.spawn((
            Mesh3d(meshes.add(om)),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.95, 0.45, 0.05),
                perceptual_roughness: 0.9,
                cull_mode: None,
                ..default()
            })),
        ));
    }

    // Red markers at every unplaced object (a hole). Each is a small box; toggle with H.
    if !holes.0.is_empty() {
        let mut positions = Vec::new();
        for h in &holes.0 {
            let center = [h[0], h[2], -h[1]]; // SA → Bevy
            push_marker_box(&mut positions, center, 3.0);
        }
        let mut marker = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        marker.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        marker.compute_flat_normals();
        commands.spawn((
            Mesh3d(meshes.add(marker)),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(1.0, 0.1, 0.1),
                emissive: LinearRgba::rgb(1.5, 0.0, 0.0), // glow so markers pop against terrain
                unlit: true,
                ..default()
            })),
            HoleMarkers,
        ));
    }

    // Sun.
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.9, 0.6, 0.0)),
    ));

    // Start at the sawmill (SA -506,-190,78 → Bevy -506,78,190), lifted and angled down.
    let yaw = 0.0f32;
    let pitch = -0.5f32;
    commands.spawn((
        Camera3d::default(),
        Transform {
            translation: Vec3::new(-506.0, 150.0, 260.0),
            rotation: Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0),
            ..default()
        },
        FlyCam { yaw, pitch },
    ));
}

/// Append a small axis-aligned box (12 triangles, non-indexed) centred at `c` with side `s`.
fn push_marker_box(out: &mut Vec<[f32; 3]>, c: [f32; 3], s: f32) {
    let h = s * 0.5;
    let corner = |i: usize| {
        [
            c[0] + if i & 1 == 0 { -h } else { h },
            c[1] + if i & 2 == 0 { -h } else { h },
            c[2] + if i & 4 == 0 { -h } else { h },
        ]
    };
    const F: [[usize; 3]; 12] = [
        [0, 1, 3],
        [0, 3, 2],
        [4, 7, 5],
        [4, 6, 7],
        [0, 4, 5],
        [0, 5, 1],
        [2, 3, 7],
        [2, 7, 6],
        [0, 2, 6],
        [0, 6, 4],
        [1, 5, 7],
        [1, 7, 3],
    ];
    for f in F {
        for &idx in &f {
            out.push(corner(idx));
        }
    }
}

/// Print the camera's SA-world position (Bevy → SA: `x, -z, y`) when `P` is pressed, so a spot in the
/// viewer can be looked up against the instance data.
fn report_position(keys: Res<ButtonInput<KeyCode>>, q: Query<&Transform, With<FlyCam>>) {
    if keys.just_pressed(KeyCode::KeyP) {
        if let Ok(t) = q.get_single() {
            let b = t.translation;
            println!("camera SA pos: {:.1}, {:.1}, {:.1}", b.x, -b.z, b.y);
        }
    }
}

/// Toggle the red hole markers with the `H` key.
fn toggle_markers(
    keys: Res<ButtonInput<KeyCode>>,
    mut q: Query<&mut Visibility, With<HoleMarkers>>,
) {
    if keys.just_pressed(KeyCode::KeyH) {
        for mut v in &mut q {
            *v = match *v {
                Visibility::Hidden => Visibility::Visible,
                _ => Visibility::Hidden,
            };
        }
    }
}

/// Map SA ground height (Z) to an RGBA vertex colour so the flat collision reads as terrain: low/water
/// blue → ground green → hills tan → tall structures near-white.
fn height_color(z: f32) -> [f32; 4] {
    const STOPS: [(f32, [f32; 3]); 5] = [
        (-20.0, [0.15, 0.22, 0.32]),
        (0.0, [0.20, 0.40, 0.45]),
        (25.0, [0.30, 0.50, 0.28]),
        (60.0, [0.62, 0.58, 0.42]),
        (120.0, [0.90, 0.90, 0.92]),
    ];
    let mut rgb = STOPS[0].1;
    for w in STOPS.windows(2) {
        let (z0, c0) = w[0];
        let (z1, c1) = w[1];
        if z <= z0 {
            rgb = c0;
            break;
        }
        if z <= z1 {
            let t = (z - z0) / (z1 - z0);
            rgb = [
                c0[0] + (c1[0] - c0[0]) * t,
                c0[1] + (c1[1] - c0[1]) * t,
                c0[2] + (c1[2] - c0[2]) * t,
            ];
            break;
        }
        rgb = c1;
    }
    [rgb[0], rgb[1], rgb[2], 1.0]
}

fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: EventReader<MouseMotion>,
    mut q: Query<(&mut Transform, &mut FlyCam)>,
) {
    let dt = time.delta().as_secs_f32();
    let Ok((mut tf, mut cam)) = q.get_single_mut() else {
        return;
    };

    // Mouse look only while the right button is held; otherwise discard motion so the view holds.
    if buttons.pressed(MouseButton::Right) {
        let mut delta = Vec2::ZERO;
        for ev in motion.read() {
            delta += ev.delta;
        }
        cam.yaw -= delta.x * 0.003;
        cam.pitch = (cam.pitch - delta.y * 0.003).clamp(-1.54, 1.54);
        tf.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    } else {
        motion.clear();
    }

    // Move relative to where we're looking. Vectors from the rotation avoid Dir3 API churn.
    let forward = tf.rotation * Vec3::NEG_Z;
    let right = tf.rotation * Vec3::X;
    let mut dir = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        dir += forward;
    }
    if keys.pressed(KeyCode::KeyS) {
        dir -= forward;
    }
    if keys.pressed(KeyCode::KeyD) {
        dir += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        dir -= right;
    }
    if keys.pressed(KeyCode::Space) {
        dir += Vec3::Y;
    }
    if keys.pressed(KeyCode::ControlLeft) {
        dir -= Vec3::Y;
    }
    if dir != Vec3::ZERO {
        let boost = if keys.pressed(KeyCode::ShiftLeft) {
            8.0
        } else {
            1.0
        };
        tf.translation += dir.normalize() * 60.0 * boost * dt;
    }
}
