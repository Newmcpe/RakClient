//! Placement: turn collision models + IPL instances into one world-space triangle mesh.
//!
//! Each [`Instance`] names a model (by id, or name as fallback) and a transform; we look up its
//! [`ColModel`], transform every vertex into the world, and append the triangles into a single
//! positions+indices buffer — ready to hand to a renderer or a spatial index / raycaster.

use std::collections::HashMap;

use crate::ipl::Instance;
use crate::ColModel;

/// A flat triangle mesh in world space: `indices` are triples into `positions`.
#[derive(Debug, Default, Clone)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Place `instances` (optionally only those in `interior`, e.g. `Some(0)` for the outdoor map) using
/// the collision `models`, producing one merged world-space mesh. Instances whose model has no
/// collision mesh, or isn't found, are skipped.
pub fn build(models: &[ColModel], instances: &[Instance], interior: Option<i32>) -> Mesh {
    // Match by NAME: a collision model's embedded id is a junk tool signature ("CED2"), so it can't be
    // trusted. Instances carry the model name (text IPL) or have it resolved from their id via IDE.
    let by_name: HashMap<String, &ColModel> = models
        .iter()
        .map(|m| (m.name.to_ascii_lowercase(), m))
        .collect();

    let mut mesh = Mesh::default();
    for inst in instances {
        if let Some(want) = interior {
            if inst.interior != want {
                continue;
            }
        }
        let model = by_name.get(&inst.model_name.to_ascii_lowercase()).copied();
        let Some(model) = model else { continue };
        if model.faces.is_empty() && model.boxes.is_empty() {
            continue; // truly empty (sphere-only or LOD) — nothing to place
        }

        // Triangle mesh.
        let base = mesh.positions.len() as u32;
        for v in &model.vertices {
            let w = inst.to_world(*v);
            mesh.positions.push([w.x, w.y, w.z]);
        }
        let vcount = model.vertices.len();
        for f in &model.faces {
            let (a, b, c) = (f.a as usize, f.b as usize, f.c as usize);
            if a.max(b).max(c) < vcount {
                mesh.indices.extend_from_slice(&[
                    base + a as u32,
                    base + b as u32,
                    base + c as u32,
                ]);
            }
        }

        // Box primitives (lampposts, poles, water/ground patches, …) — a huge share of the map's
        // collision is box-only, so emit each as 12 triangles or the world is full of holes.
        for b in &model.boxes {
            push_box(&mut mesh, |v| inst.to_world(v), b.min, b.max);
        }
    }
    mesh
}

/// Append a collision model's mesh + boxes into `mesh`, transforming each local vertex with `tf`.
fn append_model(mesh: &mut Mesh, model: &ColModel, tf: impl Fn(crate::Vec3) -> crate::Vec3) {
    let base = mesh.positions.len() as u32;
    for v in &model.vertices {
        let w = tf(*v);
        mesh.positions.push([w.x, w.y, w.z]);
    }
    let vcount = model.vertices.len();
    for f in &model.faces {
        let (a, b, c) = (f.a as usize, f.b as usize, f.c as usize);
        if a.max(b).max(c) < vcount {
            mesh.indices
                .extend_from_slice(&[base + a as u32, base + b as u32, base + c as u32]);
        }
    }
    for b in &model.boxes {
        push_box(mesh, &tf, b.min, b.max);
    }
}

/// Place server-streamed `CreateObject` instances — `(model id, world position, euler° rotation)` —
/// resolving each id to a collision model via the IDE name map. Returned as its own mesh so callers
/// can overlay Arizona's streamed custom map distinctly from the vanilla base.
pub fn place_objects(
    models: &[ColModel],
    ide: &HashMap<i32, String>,
    objects: &[(i32, crate::Vec3, crate::Vec3)],
) -> Mesh {
    let by_name: HashMap<String, &ColModel> = models
        .iter()
        .map(|m| (m.name.to_ascii_lowercase(), m))
        .collect();
    let mut mesh = Mesh::default();
    for (model_id, pos, euler) in objects {
        let Some(name) = ide.get(model_id) else {
            continue;
        };
        let Some(model) = by_name.get(&name.to_ascii_lowercase()).copied() else {
            continue;
        };
        append_model(&mut mesh, model, |v| euler_world(*pos, *euler, v));
    }
    mesh
}

/// World-place a local vertex under a `CreateObject` euler rotation (degrees) then translation.
/// SA applies the rotation as Rz·Ry·Rx (roll X, then pitch Y, then yaw Z).
fn euler_world(pos: crate::Vec3, euler_deg: crate::Vec3, v: crate::Vec3) -> crate::Vec3 {
    let (sx, cx) = euler_deg.x.to_radians().sin_cos();
    let (sy, cy) = euler_deg.y.to_radians().sin_cos();
    let (sz, cz) = euler_deg.z.to_radians().sin_cos();
    let (x1, y1, z1) = (v.x, v.y * cx - v.z * sx, v.y * sx + v.z * cx); // Rx
    let (x2, y2, z2) = (x1 * cy + z1 * sy, y1, -x1 * sy + z1 * cy); // Ry
    let (x3, y3, z3) = (x2 * cz - y2 * sz, x2 * sz + y2 * cz, z2); // Rz
    crate::Vec3::new(pos.x + x3, pos.y + y3, pos.z + z3)
}

/// Append an axis-aligned collision box (8 corners, 12 triangles), transforming each corner with `tf`.
fn push_box(
    mesh: &mut Mesh,
    tf: impl Fn(crate::Vec3) -> crate::Vec3,
    min: crate::Vec3,
    max: crate::Vec3,
) {
    use crate::Vec3;
    let base = mesh.positions.len() as u32;
    // Corners indexed by bits (x,y,z) of 0..8.
    for i in 0..8u32 {
        let c = Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        );
        let w = tf(c);
        mesh.positions.push([w.x, w.y, w.z]);
    }
    // 12 triangles, two per face (winding isn't critical — the material is double-sided).
    const FACES: [[u32; 3]; 12] = [
        [0, 1, 3],
        [0, 3, 2], // -Z bottom
        [4, 7, 5],
        [4, 6, 7], // +Z top
        [0, 4, 5],
        [0, 5, 1], // -Y
        [2, 3, 7],
        [2, 7, 6], // +Y
        [0, 2, 6],
        [0, 6, 4], // -X
        [1, 5, 7],
        [1, 7, 3], // +X
    ];
    for f in FACES {
        mesh.indices
            .extend_from_slice(&[base + f[0], base + f[1], base + f[2]]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::col::{ColModel, ColTriangle};
    use crate::ipl::Instance;
    use crate::Vec3;

    fn unit_tri_model(id: i16) -> ColModel {
        ColModel {
            name: "tri".into(),
            model_id: id,
            version: 2,
            bound_min: Vec3::new(0.0, 0.0, 0.0),
            bound_max: Vec3::new(1.0, 1.0, 0.0),
            bound_center: Vec3::new(0.0, 0.0, 0.0),
            bound_radius: 1.0,
            spheres: vec![],
            boxes: vec![],
            vertices: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            faces: vec![ColTriangle {
                a: 0,
                b: 1,
                c: 2,
                material: 0,
            }],
        }
    }

    #[test]
    fn places_instance_by_id_and_translates() {
        let models = vec![unit_tri_model(500)];
        let insts = vec![Instance {
            model_id: 500,
            model_name: "tri".into(),
            interior: 0,
            position: Vec3::new(10.0, 20.0, 30.0),
            rotation: [0.0, 0.0, 0.0, 1.0],
            lod: -1,
        }];
        let mesh = build(&models, &insts, Some(0));
        assert_eq!(mesh.triangle_count(), 1);
        assert_eq!(mesh.positions[0], [10.0, 20.0, 30.0]); // local (0,0,0) → position
        assert_eq!(mesh.positions[1], [11.0, 20.0, 30.0]); // local (1,0,0) → +x
    }

    #[test]
    fn interior_filter_and_name_fallback() {
        // Model id 0 forces the name fallback; interior filter drops the indoor instance.
        let models = vec![unit_tri_model(0)];
        let insts = vec![
            Instance {
                model_id: 999,
                model_name: "TRI".into(), // matches by name, case-insensitively
                interior: 0,
                position: Vec3::new(0.0, 0.0, 0.0),
                rotation: [0.0, 0.0, 0.0, 1.0],
                lod: -1,
            },
            Instance {
                model_id: 999,
                model_name: "tri".into(),
                interior: 3, // filtered out
                position: Vec3::new(0.0, 0.0, 0.0),
                rotation: [0.0, 0.0, 0.0, 1.0],
                lod: -1,
            },
        ];
        let mesh = build(&models, &insts, Some(0));
        assert_eq!(mesh.triangle_count(), 1);
    }
}
