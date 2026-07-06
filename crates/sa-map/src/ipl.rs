//! Text IPL parser — where each map object is placed in the world.
//!
//! The `inst` section of a `.IPL` file lists one instance per line:
//! ```text
//!   ID, ModelName, Interior, PosX, PosY, PosZ, RotX, RotY, RotZ, RotW, LOD
//! ```
//! `ID` is the model id (matches [`crate::ColModel::model_id`]), `Interior` selects a world layer (0 =
//! the outdoor map), position is in world metres, and rotation is a quaternion. SA stores the rotation
//! such that the placement uses its **conjugate** — [`Instance::rotate`] applies that so a local vertex
//! lands correctly in the world. Lines outside `inst`/`end`, blanks, and `#` comments are ignored.

use crate::{Reader, Result, Vec3};

/// One placed map object: which collision model, where, and how it's oriented.
#[derive(Debug, Clone)]
pub struct Instance {
    pub model_id: i32,
    pub model_name: String,
    pub interior: i32,
    pub position: Vec3,
    /// Rotation quaternion as stored in the IPL, `(x, y, z, w)`.
    pub rotation: [f32; 4],
    /// LOD instance index, or `-1` for none.
    pub lod: i32,
}

impl Instance {
    /// Transform a model-local vertex into world space: `position + conj(rotation) · v`.
    ///
    /// SA's stored quaternion rotates world→local, so placing a local vertex uses the conjugate
    /// (negate the vector part). Standard `q·v·q⁻¹` expanded via `t = 2·(qxyz × v)`.
    pub fn to_world(&self, v: Vec3) -> Vec3 {
        let [qx, qy, qz, qw] = self.rotation;
        // Conjugate: negate the vector part.
        let (qx, qy, qz) = (-qx, -qy, -qz);
        let vv = [v.x, v.y, v.z];
        let q = [qx, qy, qz];
        let cross = |a: [f32; 3], b: [f32; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let t = cross(q, vv).map(|c| c * 2.0);
        let qxt = cross(q, t);
        Vec3::new(
            self.position.x + vv[0] + qw * t[0] + qxt[0],
            self.position.y + vv[1] + qw * t[1] + qxt[1],
            self.position.z + vv[2] + qw * t[2] + qxt[2],
        )
    }
}

/// Parse the `inst` section of a text IPL. Unknown sections, comments, and malformed lines are skipped
/// so a stray line never aborts the whole file.
pub fn parse(text: &str) -> Vec<Instance> {
    let mut out = Vec::new();
    let mut in_inst = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !in_inst {
            if line.eq_ignore_ascii_case("inst") {
                in_inst = true;
            }
            continue;
        }
        if line.eq_ignore_ascii_case("end") {
            in_inst = false;
            continue;
        }
        if let Some(inst) = parse_inst_line(line) {
            out.push(inst);
        }
    }
    out
}

/// Parse a binary (`bnry`) IPL — the streaming placement files embedded in `gta3.img` that carry the
/// bulk of the map. Header: `"bnry"`, INST count at +4, INST-section offset at +28; each INST entry is
/// 40 bytes (position, rotation quaternion, model id, interior, LOD). Binary instances have **no
/// name** — resolve it from the id via an IDE map. Returns empty on a bad/short header.
pub fn parse_binary(data: &[u8]) -> Vec<Instance> {
    if data.len() < 32 || &data[0..4] != b"bnry" {
        return Vec::new();
    }
    let num = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let offset = u32::from_le_bytes(data[28..32].try_into().unwrap()) as usize;

    let mut out = Vec::with_capacity(num.min(1 << 16));
    let mut r = Reader::at(data, offset);
    for _ in 0..num {
        let parsed = (|| -> Result<Instance> {
            let position = r.vec3()?;
            let rotation = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
            let model_id = r.u32()? as i32;
            // The interior field's high bits are flags (fastman92-extended maps use 0x100, 0x400, …);
            // the real interior id is the low byte. Without masking, the `interior == 0` outdoor filter
            // drops thousands of genuinely-exterior objects (bridge ramps, etc.).
            let interior = (r.u32()? & 0xFF) as i32;
            let lod = r.u32()? as i32;
            Ok(Instance {
                model_id,
                model_name: String::new(),
                interior,
                position,
                rotation,
                lod,
            })
        })();
        match parsed {
            Ok(inst) => out.push(inst),
            Err(_) => break, // truncated section
        }
    }
    out
}

fn parse_inst_line(line: &str) -> Option<Instance> {
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    if f.len() < 11 {
        return None;
    }
    Some(Instance {
        model_id: f[0].parse().ok()?,
        model_name: f[1].to_string(),
        // Mask flag bits (see parse_binary): the real interior id is the low byte.
        interior: f[2].parse::<i32>().ok()? & 0xFF,
        position: Vec3::new(f[3].parse().ok()?, f[4].parse().ok()?, f[5].parse().ok()?),
        rotation: [
            f[6].parse().ok()?,
            f[7].parse().ok()?,
            f[8].parse().ok()?,
            f[9].parse().ok()?,
        ],
        lod: f[10].parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inst_section_only() {
        let text = "\
# comment
inst
1337, dummy, 0, 10.0, 20.0, 30.0, 0.0, 0.0, 0.0, 1.0, -1
2, other, 5, 1.5, 2.5, 3.5, 0, 0, 0.7071, 0.7071, -1
end
cull
9999, ignored, 0, 0,0,0,0,0,0,1,-1
";
        let insts = parse(text);
        assert_eq!(insts.len(), 2);
        assert_eq!(insts[0].model_id, 1337);
        assert_eq!(insts[0].model_name, "dummy");
        assert_eq!(insts[0].position, Vec3::new(10.0, 20.0, 30.0));
        assert_eq!(insts[1].interior, 5);
    }

    #[test]
    fn identity_rotation_is_pure_translation() {
        let inst = Instance {
            model_id: 1,
            model_name: "x".into(),
            interior: 0,
            position: Vec3::new(100.0, 200.0, 5.0),
            rotation: [0.0, 0.0, 0.0, 1.0],
            lod: -1,
        };
        assert_eq!(
            inst.to_world(Vec3::new(1.0, 2.0, 3.0)),
            Vec3::new(101.0, 202.0, 8.0)
        );
    }

    #[test]
    fn quarter_turn_about_z_rotates_xy() {
        // Stored quaternion (0,0,sin45,cos45); placement uses its conjugate → -90° about Z, mapping
        // local +X to world -Y. Values within float tolerance.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let inst = Instance {
            model_id: 1,
            model_name: "x".into(),
            interior: 0,
            position: Vec3::new(0.0, 0.0, 0.0),
            rotation: [0.0, 0.0, s, s],
            lod: -1,
        };
        let w = inst.to_world(Vec3::new(1.0, 0.0, 0.0));
        assert!((w.x - 0.0).abs() < 1e-5, "x={}", w.x);
        assert!((w.y + 1.0).abs() < 1e-5, "y={}", w.y);
    }
}
