//! Bevy flythrough of a PREBAKED GTA SA world scene.
//!
//! The heavy world assembly (parsing every IMG + streamer bin + DFF, ~tens of
//! seconds) is done ONCE offline by `samap scene`, which dumps a `.scene` file.
//! This viewer only reads that dump and uploads it — so it opens fast instead of
//! freezing on launch. Coordinates are SA (Z-up), converted to Bevy (Y-up):
//! `(x, y, z) → (x, z, -y)`.
//!
//! Bake:  cargo run --release -p sa-map --bin samap -- scene <gta3.img> <data> world.scene [objects.csv]
//! View:  cargo run --release --manifest-path crates/sa-viewer/Cargo.toml -- world.scene [nav.nav]
//!
//! Controls: CLICK to look (Esc release) · WASD move · Space/Ctrl up/down · Shift boost.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::render::mesh::PrimitiveTopology;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::window::{CursorGrabMode, PrimaryWindow};

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

/// Walkable navmesh detail triangles (SA space), rendered as a translucent green
/// carpet slightly above the ground so coverage reads at a glance.
#[derive(Resource)]
struct NavOverlay(Vec<[[f32; 3]; 3]>);

/// Interactive navmesh path tester: `1` sets the start, `2` sets the end (and
/// plans), `C` clears. Points are picked by raycasting the camera crosshair
/// against the navmesh detail triangles; the planned route draws as gizmo lines.
#[derive(Resource, Default)]
struct PathTest {
    query: Option<sa_nav::NavQuery>,
    start: Option<[f32; 3]>,
    end: Option<[f32; 3]>,
    waypoints: Vec<[f32; 3]>,
}

/// Parse an `SA_NAV_TEST` "x1,y1,z1:x2,y2,z2" spec into (start, end) SA points.
fn parse_test_spec(spec: &str) -> Option<([f32; 3], [f32; 3])> {
    let (a, b) = spec.split_once(':')?;
    let pt = |s: &str| -> Option<[f32; 3]> {
        let v: Vec<f32> = s.split(',').filter_map(|c| c.trim().parse().ok()).collect();
        (v.len() == 3).then(|| [v[0], v[1], v[2]])
    };
    Some((pt(a)?, pt(b)?))
}

/// The fly camera's accumulated look angles (radians).
#[derive(Component, Default)]
struct FlyCam {
    yaw: f32,
    pitch: f32,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let scene_path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!(
                "usage: sa-viewer <world.scene> [nav.nav]\n  \
                 bake first: samap scene <gta3.img> <data-dir> world.scene [objects.csv]"
            );
            std::process::exit(2);
        }
    };

    println!("loading scene {scene_path}…");
    let t0 = std::time::Instant::now();
    let scene = match std::fs::File::open(&scene_path)
        .map_err(|e| e.to_string())
        .and_then(|f| {
            sa_map::scene::Scene::load(&mut std::io::BufReader::new(f)).map_err(|e| e.to_string())
        }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load scene {scene_path}: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "scene loaded in {:.1?}: base {} tris, streamed {} tris, {} holes",
        t0.elapsed(),
        scene.base.triangle_count(),
        scene.streamed.triangle_count(),
        scene.holes.len(),
    );
    let mesh = scene.base;
    let streamed = scene.streamed;
    let holes = scene.holes;

    // Optional overlay: a `.nav` navmesh from `navgen` — walkable detail triangles in SA space,
    // plus the pathfinding view for the interactive path tester (keys 1/2/C).
    let mut path_test = PathTest::default();
    let nav_tris: Vec<[[f32; 3]; 3]> = if let Some(nav_path) = args.next() {
        match std::fs::File::open(&nav_path)
            .map_err(|e| e.to_string())
            .and_then(|f| {
                sa_nav::NavMesh::load(&mut std::io::BufReader::new(f)).map_err(|e| e.to_string())
            }) {
            Ok(nav) => {
                println!(
                    "navmesh overlay: {} polys, {} detail tris",
                    nav.polys.len(),
                    nav.detail_tris.len()
                );
                let tris = nav
                    .detail_tris
                    .iter()
                    .map(|t| t.map(|i| nav.detail_verts[i as usize]))
                    .collect();
                let query = sa_nav::NavQuery::new(nav);
                // SA_NAV_TEST="x1,y1,z1:x2,y2,z2" plants a path at startup — deterministic
                // verification without aiming the crosshair by hand.
                if let Ok(spec) = std::env::var("SA_NAV_TEST") {
                    if let Some((a, b)) = parse_test_spec(&spec) {
                        path_test.start = Some(a);
                        path_test.end = Some(b);
                        match query.find_path(a, b) {
                            Some(pts) => {
                                println!("path(test): {} waypoints", pts.len());
                                path_test.waypoints = pts;
                            }
                            None => println!("path(test): NO ROUTE"),
                        }
                    }
                }
                path_test.query = Some(query);
                tris
            }
            Err(e) => {
                eprintln!("could not load navmesh {nav_path}: {e}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    println!(
        "loaded {} triangles ({} vertices), {} unplaced-object markers — opening viewer…\ncontrols: CLICK to look (Esc release) · WASD move · Space/Ctrl up/down · Shift boost · H toggle markers · P print pos · 1/2 path start/end · C clear path",
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
        .insert_resource(NavOverlay(nav_tris))
        .insert_resource(path_test)
        .add_systems(Startup, (setup, spawn_crosshair))
        .add_systems(
            Update,
            (
                cursor_grab,
                fly,
                toggle_markers,
                report_position,
                path_pick,
                draw_path,
            ),
        )
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    geometry: Res<Geometry>,
    holes: Res<Holes>,
    streamed: Res<StreamedObjects>,
    nav: Res<NavOverlay>,
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

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
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
        let mut om = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
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

    // Navmesh overlay: walkable detail triangles as a translucent green carpet, lifted 0.15 m so
    // it doesn't z-fight the ground it follows.
    if !nav.0.is_empty() {
        let mut positions = Vec::with_capacity(nav.0.len() * 3);
        for tri in &nav.0 {
            for p in tri {
                positions.push([p[0], p[2] + 0.15, -p[1]]); // SA → Bevy
            }
        }
        let mut nm = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        nm.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        nm.compute_flat_normals();
        commands.spawn((
            Mesh3d(meshes.add(nm)),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgba(0.1, 0.9, 0.25, 0.55),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
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
        let mut marker = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
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

    // Start at the sawmill (SA -506,-190,78 → Bevy -506,78,190), high above the giant-fir canopy
    // (their render meshes reach SA z≈190) and angled down so the spawn view is the clearing, not
    // the inside of a trunk.
    let yaw = 0.0f32;
    let pitch = -0.9f32;
    commands.spawn((
        Camera3d::default(),
        Transform {
            translation: Vec3::new(-506.0, 300.0, 330.0),
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

/// Cursor capture, click-to-grab / Escape-to-release (the safe editor/FPS pattern).
///
/// Grabbing at STARTUP hangs the whole desktop on Windows: `Locked` warps + hides
/// the cursor every frame, and if the window isn't focused yet the pointer looks
/// frozen system-wide. So we start with a FREE cursor and only capture on a LEFT
/// CLICK inside the window; Escape (or losing focus) releases it.
fn cursor_grab(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut window: Query<&mut Window, With<PrimaryWindow>>,
) {
    let Ok(mut w) = window.get_single_mut() else {
        return;
    };
    let grabbed = w.cursor_options.grab_mode != CursorGrabMode::None;
    if !grabbed && buttons.just_pressed(MouseButton::Left) {
        w.cursor_options.grab_mode = CursorGrabMode::Locked;
        w.cursor_options.visible = false;
    } else if grabbed && keys.just_pressed(KeyCode::Escape) {
        w.cursor_options.grab_mode = CursorGrabMode::None;
        w.cursor_options.visible = true;
    }
}

/// A fixed centre-screen aiming reticle: the crosshair the raycast picker (`1`/`2`)
/// shoots through, so the placed marker always lands exactly where this dot points.
#[derive(Component)]
struct Crosshair;

fn spawn_crosshair(mut commands: Commands) {
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            // Let clicks/pointer pass through — this is a HUD overlay, not a button.
            PickingBehavior::IGNORE,
            Crosshair,
        ))
        .with_children(|p| {
            p.spawn((
                Node {
                    width: Val::Px(6.0),
                    height: Val::Px(6.0),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BorderColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.9)),
            ));
        });
}

fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion: EventReader<MouseMotion>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut q: Query<(&mut Transform, &mut FlyCam)>,
) {
    let dt = time.delta().as_secs_f32();
    let Ok((mut tf, mut cam)) = q.get_single_mut() else {
        return;
    };

    // Mouse-look whenever the cursor is grabbed (FPS-style); with it released
    // (Escape) discard motion so the view holds while the pointer is free.
    let grabbed = window
        .get_single()
        .is_ok_and(|w| w.cursor_options.grab_mode != CursorGrabMode::None);
    if grabbed {
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

/// SA -> Bevy point conversion (matches the mesh build in `setup`).
fn sa_to_bevy(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[2], -p[1])
}

/// Pick path endpoints with the camera crosshair: `1` = start, `2` = end (plans
/// immediately when both are set), `C` = clear. The pick ray is intersected with
/// the navmesh detail triangles in SA space, so a picked point is always on (or a
/// hair above) the walkable surface `NavQuery::locate` expects.
fn path_pick(
    keys: Res<ButtonInput<KeyCode>>,
    q: Query<&Transform, With<FlyCam>>,
    nav: Res<NavOverlay>,
    mut test: ResMut<PathTest>,
) {
    let set_start = keys.just_pressed(KeyCode::Digit1);
    let set_end = keys.just_pressed(KeyCode::Digit2);
    if keys.just_pressed(KeyCode::KeyC) {
        test.start = None;
        test.end = None;
        test.waypoints.clear();
        println!("path: cleared");
        return;
    }
    if !set_start && !set_end {
        return;
    }
    if test.query.is_none() {
        println!("path: no navmesh loaded (pass a .nav as the 4th argument)");
        return;
    }
    let Ok(cam) = q.get_single() else {
        return;
    };
    // Camera ray in SA space: Bevy (x, y, z) -> SA (x, -z, y) for both origin and direction.
    let o = cam.translation;
    let d = cam.rotation * Vec3::NEG_Z;
    let origin = [o.x, -o.z, o.y];
    let dir = [d.x, -d.z, d.y];
    let Some(hit) = raycast_tris(origin, dir, &nav.0) else {
        println!("path: crosshair is not over the navmesh");
        return;
    };
    if set_start {
        test.start = Some(hit);
        println!("path: start {:.1},{:.1},{:.1}", hit[0], hit[1], hit[2]);
    } else {
        test.end = Some(hit);
        println!("path: end {:.1},{:.1},{:.1}", hit[0], hit[1], hit[2]);
    }
    test.waypoints.clear();
    if let (Some(start), Some(end), Some(query)) = (test.start, test.end, test.query.as_ref()) {
        match query.find_path(start, end) {
            Some(points) => {
                let mut length = 0.0;
                let mut prev = start;
                for w in &points {
                    length += ((w[0] - prev[0]).powi(2)
                        + (w[1] - prev[1]).powi(2)
                        + (w[2] - prev[2]).powi(2))
                    .sqrt();
                    prev = *w;
                }
                println!("path: {} waypoints, {:.1} m", points.len(), length);
                test.waypoints = points;
            }
            None => println!("path: NO ROUTE between the picked points"),
        }
    }
}

/// Nearest intersection of a ray with a triangle soup (Möller–Trumbore), SA space.
fn raycast_tris(origin: [f32; 3], dir: [f32; 3], tris: &[[[f32; 3]; 3]]) -> Option<[f32; 3]> {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let mut best: Option<f32> = None;
    for t in tris {
        let e1 = sub(t[1], t[0]);
        let e2 = sub(t[2], t[0]);
        let h = cross(dir, e2);
        let a = dot(e1, h);
        if a.abs() < 1e-7 {
            continue;
        }
        let f = 1.0 / a;
        let sv = sub(origin, t[0]);
        let u = f * dot(sv, h);
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let qv = cross(sv, e1);
        let v = f * dot(dir, qv);
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let tt = f * dot(e2, qv);
        if tt > 0.01 && best.is_none_or(|b| tt < b) {
            best = Some(tt);
        }
    }
    best.map(|tt| {
        [
            origin[0] + dir[0] * tt,
            origin[1] + dir[1] * tt,
            origin[2] + dir[2] * tt,
        ]
    })
}

/// Draw the picked endpoints and the planned route as immediate-mode gizmos,
/// lifted ~0.4 m so the line reads above the green navmesh carpet.
fn draw_path(test: Res<PathTest>, mut gizmos: Gizmos) {
    let lift = Vec3::Y * 0.4;
    if let Some(s) = test.start {
        gizmos.sphere(
            Isometry3d::from_translation(sa_to_bevy(s) + lift),
            0.5,
            Color::srgb(0.2, 0.4, 1.0),
        );
    }
    if let Some(e) = test.end {
        gizmos.sphere(
            Isometry3d::from_translation(sa_to_bevy(e) + lift),
            0.5,
            Color::srgb(1.0, 0.2, 0.2),
        );
    }
    if test.waypoints.is_empty() {
        return;
    }
    let mut prev = match test.start {
        Some(s) => sa_to_bevy(s) + lift,
        None => return,
    };
    for w in &test.waypoints {
        let next = sa_to_bevy(*w) + lift;
        gizmos.line(prev, next, Color::srgb(1.0, 0.95, 0.1));
        gizmos.sphere(
            Isometry3d::from_translation(next),
            0.18,
            Color::srgb(1.0, 0.7, 0.1),
        );
        prev = next;
    }
}
