//! Native typed sync-packet structs, replacing the LuaJIT `ffi` structs in `synchronization.lua`.
//!
//! Two encoding families are represented here:
//! - **Bit-packed** sync (`PlayerSyncData`, `VehicleSyncData`): the SA-MP sender uses the bitstream
//!   field helpers (optional-value bool flags, `normQuat`, `compressedVector`, compressed
//!   health/armor) — ported from `events/handlers.lua` `packet_player_sync_*`/`packet_vehicle_sync_*`.
//! - **Raw struct** sync (`AimSyncData`, `BulletSyncData`, `TrailerSyncData`, `UnoccupiedSyncData`,
//!   `PassengerSyncData`): SA-MP reads/writes these as a flat byte buffer (`read_sync_data`), so each
//!   field is a plain byte-aligned read in declaration order, exactly matching the packed C structs.
//!   Bitfield bytes (e.g. `camExtZoom:6|weaponState:2`) are kept as the raw combined `u8`.

use crate::bitstream::{BitStreamReader, BitStreamWriter};
use crate::codec::{Encode, Packet};
use crate::ids::SyncPacketId;
use crate::{Quaternion, Result, Vector3};

/// `hp = min((b >> 4) * 7, 100)`, `armor = min((b & 0xF) * 7, 100)` — port of
/// `utils.decompress_health_and_armor`.
fn decompress_health_and_armor(byte: u8) -> (u8, u8) {
    let hp = ((byte >> 4) as u16 * 7).min(100) as u8;
    let armor = ((byte & 0x0F) as u16 * 7).min(100) as u8;
    (hp, armor)
}

/// Inverse of [`decompress_health_and_armor`] — port of `utils.compress_health_and_armor`. Values at
/// or above 100 saturate the nibble; otherwise the nibble is `value / 7` (integer-truncated, matching
/// the Lua bit ops). Only multiples of 7 (and `>= 100`) survive a decompress→compress round-trip.
fn compress_health_and_armor(health: u8, armor: u8) -> u8 {
    let hp = if health >= 100 {
        0xF0
    } else {
        (health / 7) << 4
    };
    let ap = if armor >= 100 {
        0x0F
    } else {
        (armor / 7) & 0x0F
    };
    hp | ap
}

/// On-foot player sync (`ID_PLAYER_SYNC`). Optional fields mirror the `bool`-flagged groups in the
/// SA-MP reader: `left_right_keys`/`up_down_keys`, the surfing group, and the animation group.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayerSyncData {
    pub player_id: u16,
    pub left_right_keys: Option<u16>,
    pub up_down_keys: Option<u16>,
    pub keys_data: u16,
    pub position: Vector3,
    pub quaternion: Quaternion,
    pub health: u8,
    pub armor: u8,
    pub weapon: u8,
    pub special_action: u8,
    pub move_speed: Vector3,
    pub surfing_vehicle_id: Option<u16>,
    pub surfing_offsets: Vector3,
    pub animation_id: Option<u16>,
    pub animation_flags: u16,
}

impl PlayerSyncData {
    pub fn decode(r: &mut BitStreamReader) -> Result<Self> {
        let player_id = r.read_u16()?;
        let left_right_keys = if r.read_bit()? {
            Some(r.read_u16()?)
        } else {
            None
        };
        let up_down_keys = if r.read_bit()? {
            Some(r.read_u16()?)
        } else {
            None
        };
        let keys_data = r.read_u16()?;
        let position = r.read_vector3()?;
        let quaternion = r.read_norm_quat()?;
        let (health, armor) = decompress_health_and_armor(r.read_u8()?);
        let weapon = r.read_u8()?;
        let special_action = r.read_u8()?;
        let move_speed = r.read_compressed_vector()?;
        let (surfing_vehicle_id, surfing_offsets) = if r.read_bit()? {
            (Some(r.read_u16()?), r.read_vector3()?)
        } else {
            (None, Vector3::default())
        };
        let (animation_id, animation_flags) = if r.read_bit()? {
            (Some(r.read_u16()?), r.read_u16()?)
        } else {
            (None, 0)
        };
        Ok(Self {
            player_id,
            left_right_keys,
            up_down_keys,
            keys_data,
            position,
            quaternion,
            health,
            armor,
            weapon,
            special_action,
            move_speed,
            surfing_vehicle_id,
            surfing_offsets,
            animation_id,
            animation_flags,
        })
    }

    pub fn write(&self, w: &mut BitStreamWriter) {
        w.write_u16(self.player_id);
        w.write_bit(self.left_right_keys.is_some());
        if let Some(v) = self.left_right_keys {
            w.write_u16(v);
        }
        w.write_bit(self.up_down_keys.is_some());
        if let Some(v) = self.up_down_keys {
            w.write_u16(v);
        }
        w.write_u16(self.keys_data);
        w.write_vector3(self.position);
        w.write_norm_quat(self.quaternion);
        w.write_u8(compress_health_and_armor(self.health, self.armor));
        w.write_u8(self.weapon);
        w.write_u8(self.special_action);
        w.write_compressed_vector(self.move_speed);
        w.write_bit(self.surfing_vehicle_id.is_some());
        if let Some(v) = self.surfing_vehicle_id {
            w.write_u16(v);
            w.write_vector3(self.surfing_offsets);
        }
        w.write_bit(self.animation_id.is_some());
        if let Some(v) = self.animation_id {
            w.write_u16(v);
            w.write_u16(self.animation_flags);
        }
    }
}

impl Packet for PlayerSyncData {
    const ID: u8 = SyncPacketId::PlayerSync as u8;
}

impl Encode for PlayerSyncData {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        self.write(&mut w);
        w.into_bytes()
    }
}

/// In-vehicle driver sync (`ID_VEHICLE_SYNC`). `train_speed` and `trailer_id` are bool-flagged
/// optionals in the SA-MP reader.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct VehicleSyncData {
    pub player_id: u16,
    pub vehicle_id: u16,
    pub left_right_keys: u16,
    pub up_down_keys: u16,
    pub keys_data: u16,
    pub quaternion: Quaternion,
    pub position: Vector3,
    pub move_speed: Vector3,
    pub vehicle_health: u16,
    pub player_health: u8,
    pub armor: u8,
    pub current_weapon: u8,
    pub siren: bool,
    pub landing_gear: bool,
    pub train_speed: Option<i32>,
    pub trailer_id: Option<u16>,
}

impl VehicleSyncData {
    pub fn decode(r: &mut BitStreamReader) -> Result<Self> {
        let player_id = r.read_u16()?;
        let vehicle_id = r.read_u16()?;
        let left_right_keys = r.read_u16()?;
        let up_down_keys = r.read_u16()?;
        let keys_data = r.read_u16()?;
        let quaternion = r.read_norm_quat()?;
        let position = r.read_vector3()?;
        let move_speed = r.read_compressed_vector()?;
        let vehicle_health = r.read_u16()?;
        let (player_health, armor) = decompress_health_and_armor(r.read_u8()?);
        let current_weapon = r.read_u8()?;
        let siren = r.read_bit()?;
        let landing_gear = r.read_bit()?;
        let train_speed = if r.read_bit()? {
            Some(r.read_i32()?)
        } else {
            None
        };
        let trailer_id = if r.read_bit()? {
            Some(r.read_u16()?)
        } else {
            None
        };
        Ok(Self {
            player_id,
            vehicle_id,
            left_right_keys,
            up_down_keys,
            keys_data,
            quaternion,
            position,
            move_speed,
            vehicle_health,
            player_health,
            armor,
            current_weapon,
            siren,
            landing_gear,
            train_speed,
            trailer_id,
        })
    }

    pub fn write(&self, w: &mut BitStreamWriter) {
        w.write_u16(self.player_id);
        w.write_u16(self.vehicle_id);
        w.write_u16(self.left_right_keys);
        w.write_u16(self.up_down_keys);
        w.write_u16(self.keys_data);
        w.write_norm_quat(self.quaternion);
        w.write_vector3(self.position);
        w.write_compressed_vector(self.move_speed);
        w.write_u16(self.vehicle_health);
        w.write_u8(compress_health_and_armor(self.player_health, self.armor));
        w.write_u8(self.current_weapon);
        w.write_bit(self.siren);
        w.write_bit(self.landing_gear);
        w.write_bit(self.train_speed.is_some());
        if let Some(v) = self.train_speed {
            w.write_i32(v);
        }
        w.write_bit(self.trailer_id.is_some());
        if let Some(v) = self.trailer_id {
            w.write_u16(v);
        }
    }
}

impl Packet for VehicleSyncData {
    const ID: u8 = SyncPacketId::VehicleSync as u8;
}

impl Encode for VehicleSyncData {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        self.write(&mut w);
        w.into_bytes()
    }
}

/// Aim/camera sync (`AimSyncData`, 31 bytes). `cam_ext_zoom_weapon_state` packs `camExtZoom:6` in the
/// low bits and `weaponState:2` in the high bits.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AimSyncData {
    pub cam_mode: u8,
    pub cam_front: Vector3,
    pub cam_pos: Vector3,
    pub aim_z: f32,
    pub cam_ext_zoom_weapon_state: u8,
    pub aspect_ratio: u8,
}

impl AimSyncData {
    pub fn decode(r: &mut BitStreamReader) -> Result<Self> {
        Ok(Self {
            cam_mode: r.read_u8()?,
            cam_front: r.read_vector3()?,
            cam_pos: r.read_vector3()?,
            aim_z: r.read_f32()?,
            cam_ext_zoom_weapon_state: r.read_u8()?,
            aspect_ratio: r.read_u8()?,
        })
    }

    pub fn write(&self, w: &mut BitStreamWriter) {
        w.write_u8(self.cam_mode);
        w.write_vector3(self.cam_front);
        w.write_vector3(self.cam_pos);
        w.write_f32(self.aim_z);
        w.write_u8(self.cam_ext_zoom_weapon_state);
        w.write_u8(self.aspect_ratio);
    }
}

impl Packet for AimSyncData {
    const ID: u8 = SyncPacketId::AimSync as u8;
}

impl Encode for AimSyncData {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        self.write(&mut w);
        w.into_bytes()
    }
}

/// Bullet/shot sync (`BulletSyncData`, 40 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BulletSyncData {
    pub target_type: u8,
    pub target_id: u16,
    pub origin: Vector3,
    pub target: Vector3,
    pub center: Vector3,
    pub weapon_id: u8,
}

impl BulletSyncData {
    pub fn decode(r: &mut BitStreamReader) -> Result<Self> {
        Ok(Self {
            target_type: r.read_u8()?,
            target_id: r.read_u16()?,
            origin: r.read_vector3()?,
            target: r.read_vector3()?,
            center: r.read_vector3()?,
            weapon_id: r.read_u8()?,
        })
    }

    pub fn write(&self, w: &mut BitStreamWriter) {
        w.write_u8(self.target_type);
        w.write_u16(self.target_id);
        w.write_vector3(self.origin);
        w.write_vector3(self.target);
        w.write_vector3(self.center);
        w.write_u8(self.weapon_id);
    }
}

impl Packet for BulletSyncData {
    const ID: u8 = SyncPacketId::BulletSync as u8;
}

impl Encode for BulletSyncData {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        self.write(&mut w);
        w.into_bytes()
    }
}

/// Trailer sync (`TrailerSyncData`, 54 bytes). `quaternion` is the raw `float[4]` mapped in
/// `x, y, z, w` order.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrailerSyncData {
    pub trailer_id: u16,
    pub position: Vector3,
    pub quaternion: Quaternion,
    pub move_speed: Vector3,
    pub turn_speed: Vector3,
}

impl TrailerSyncData {
    pub fn decode(r: &mut BitStreamReader) -> Result<Self> {
        let trailer_id = r.read_u16()?;
        let position = r.read_vector3()?;
        let quaternion = Quaternion {
            x: r.read_f32()?,
            y: r.read_f32()?,
            z: r.read_f32()?,
            w: r.read_f32()?,
        };
        let move_speed = r.read_vector3()?;
        let turn_speed = r.read_vector3()?;
        Ok(Self {
            trailer_id,
            position,
            quaternion,
            move_speed,
            turn_speed,
        })
    }

    pub fn write(&self, w: &mut BitStreamWriter) {
        w.write_u16(self.trailer_id);
        w.write_vector3(self.position);
        w.write_f32(self.quaternion.x);
        w.write_f32(self.quaternion.y);
        w.write_f32(self.quaternion.z);
        w.write_f32(self.quaternion.w);
        w.write_vector3(self.move_speed);
        w.write_vector3(self.turn_speed);
    }
}

impl Packet for TrailerSyncData {
    const ID: u8 = SyncPacketId::TrailerSync as u8;
}

impl Encode for TrailerSyncData {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        self.write(&mut w);
        w.into_bytes()
    }
}

/// Unoccupied-vehicle sync (`UnoccupiedSyncData`, 67 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UnoccupiedSyncData {
    pub vehicle_id: u16,
    pub seat_id: u8,
    pub roll: Vector3,
    pub direction: Vector3,
    pub position: Vector3,
    pub move_speed: Vector3,
    pub turn_speed: Vector3,
    pub vehicle_health: f32,
}

impl UnoccupiedSyncData {
    pub fn decode(r: &mut BitStreamReader) -> Result<Self> {
        Ok(Self {
            vehicle_id: r.read_u16()?,
            seat_id: r.read_u8()?,
            roll: r.read_vector3()?,
            direction: r.read_vector3()?,
            position: r.read_vector3()?,
            move_speed: r.read_vector3()?,
            turn_speed: r.read_vector3()?,
            vehicle_health: r.read_f32()?,
        })
    }

    pub fn write(&self, w: &mut BitStreamWriter) {
        w.write_u16(self.vehicle_id);
        w.write_u8(self.seat_id);
        w.write_vector3(self.roll);
        w.write_vector3(self.direction);
        w.write_vector3(self.position);
        w.write_vector3(self.move_speed);
        w.write_vector3(self.turn_speed);
        w.write_f32(self.vehicle_health);
    }
}

impl Packet for UnoccupiedSyncData {
    const ID: u8 = SyncPacketId::UnoccupiedSync as u8;
}

impl Encode for UnoccupiedSyncData {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        self.write(&mut w);
        w.into_bytes()
    }
}

/// In-vehicle passenger sync (`PassengerSyncData`, 24 bytes). `seat_drive_cuff` packs
/// `seatId:6|driveBy:1|cuffed:1`; `weapon_special` packs `currentWeapon:6|specialKey:2`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PassengerSyncData {
    pub vehicle_id: u16,
    pub seat_drive_cuff: u8,
    pub weapon_special: u8,
    pub health: u8,
    pub armor: u8,
    pub left_right_keys: u16,
    pub up_down_keys: u16,
    pub keys_data: u16,
    pub position: Vector3,
}

impl PassengerSyncData {
    pub fn decode(r: &mut BitStreamReader) -> Result<Self> {
        Ok(Self {
            vehicle_id: r.read_u16()?,
            seat_drive_cuff: r.read_u8()?,
            weapon_special: r.read_u8()?,
            health: r.read_u8()?,
            armor: r.read_u8()?,
            left_right_keys: r.read_u16()?,
            up_down_keys: r.read_u16()?,
            keys_data: r.read_u16()?,
            position: r.read_vector3()?,
        })
    }

    pub fn write(&self, w: &mut BitStreamWriter) {
        w.write_u16(self.vehicle_id);
        w.write_u8(self.seat_drive_cuff);
        w.write_u8(self.weapon_special);
        w.write_u8(self.health);
        w.write_u8(self.armor);
        w.write_u16(self.left_right_keys);
        w.write_u16(self.up_down_keys);
        w.write_u16(self.keys_data);
        w.write_vector3(self.position);
    }
}

impl Packet for PassengerSyncData {
    const ID: u8 = SyncPacketId::PassengerSync as u8;
}

impl Encode for PassengerSyncData {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        self.write(&mut w);
        w.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec3(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3 { x, y, z }
    }

    #[test]
    fn health_armor_compression_roundtrips_for_multiples_of_7() {
        for raw in 0u8..=255 {
            let (hp, ap) = decompress_health_and_armor(raw);
            let recompressed = compress_health_and_armor(hp, ap);
            assert_eq!(
                decompress_health_and_armor(recompressed),
                (hp, ap),
                "raw {raw:#04x}"
            );
        }
    }

    #[test]
    fn player_sync_roundtrips() {
        let data = PlayerSyncData {
            player_id: 42,
            left_right_keys: Some(0x1234),
            up_down_keys: None,
            keys_data: 0xABCD,
            position: vec3(100.5, -200.25, 15.0),
            quaternion: Quaternion {
                x: 0.5,
                y: -0.5,
                z: 0.5,
                w: 0.5,
            },
            health: 98,
            armor: 70,
            weapon: 24,
            special_action: 3,
            move_speed: vec3(1.0, -2.0, 0.5),
            surfing_vehicle_id: Some(7),
            surfing_offsets: vec3(0.1, 0.2, 0.3),
            animation_id: Some(1500),
            animation_flags: 0x00FF,
        };
        let mut w = BitStreamWriter::new();
        data.write(&mut w);
        let bytes = w.into_bytes();
        let mut r = BitStreamReader::new(&bytes);
        let got = PlayerSyncData::decode(&mut r).unwrap();

        assert_eq!(got.player_id, data.player_id);
        assert_eq!(got.left_right_keys, data.left_right_keys);
        assert_eq!(got.up_down_keys, data.up_down_keys);
        assert_eq!(got.keys_data, data.keys_data);
        assert_eq!(got.position, data.position);
        assert_eq!(got.health, data.health);
        assert_eq!(got.armor, data.armor);
        assert_eq!(got.weapon, data.weapon);
        assert_eq!(got.special_action, data.special_action);
        assert_eq!(got.surfing_vehicle_id, data.surfing_vehicle_id);
        assert_eq!(got.surfing_offsets, data.surfing_offsets);
        assert_eq!(got.animation_id, data.animation_id);
        assert_eq!(got.animation_flags, data.animation_flags);
        assert!((got.quaternion.x - data.quaternion.x).abs() < 0.001);
        assert!((got.move_speed.x - data.move_speed.x).abs() < 0.01);
    }

    #[test]
    fn player_sync_roundtrips_with_no_optionals() {
        let data = PlayerSyncData {
            player_id: 1,
            keys_data: 0,
            position: vec3(0.0, 0.0, 3.0),
            quaternion: Quaternion {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 1.0,
            },
            health: 100,
            armor: 0,
            ..Default::default()
        };
        let mut w = BitStreamWriter::new();
        data.write(&mut w);
        let bytes = w.into_bytes();
        let mut r = BitStreamReader::new(&bytes);
        let got = PlayerSyncData::decode(&mut r).unwrap();
        assert_eq!(got.left_right_keys, None);
        assert_eq!(got.up_down_keys, None);
        assert_eq!(got.surfing_vehicle_id, None);
        assert_eq!(got.animation_id, None);
        assert_eq!(got.health, 100);
    }

    #[test]
    fn vehicle_sync_roundtrips() {
        let data = VehicleSyncData {
            player_id: 5,
            vehicle_id: 411,
            left_right_keys: 0x0102,
            up_down_keys: 0x0304,
            keys_data: 0x0506,
            quaternion: Quaternion {
                x: 0.5,
                y: 0.5,
                z: -0.5,
                w: 0.5,
            },
            position: vec3(1000.0, 2000.0, 20.0),
            move_speed: vec3(0.3, -0.4, 0.0),
            vehicle_health: 950,
            player_health: 91,
            armor: 49,
            current_weapon: 0,
            siren: true,
            landing_gear: false,
            train_speed: Some(-123),
            trailer_id: Some(412),
        };
        let mut w = BitStreamWriter::new();
        data.write(&mut w);
        let bytes = w.into_bytes();
        let mut r = BitStreamReader::new(&bytes);
        let got = VehicleSyncData::decode(&mut r).unwrap();

        assert_eq!(got.player_id, data.player_id);
        assert_eq!(got.vehicle_id, data.vehicle_id);
        assert_eq!(got.left_right_keys, data.left_right_keys);
        assert_eq!(got.up_down_keys, data.up_down_keys);
        assert_eq!(got.keys_data, data.keys_data);
        assert_eq!(got.vehicle_health, data.vehicle_health);
        assert_eq!(got.player_health, data.player_health);
        assert_eq!(got.armor, data.armor);
        assert_eq!(got.current_weapon, data.current_weapon);
        assert_eq!(got.siren, data.siren);
        assert_eq!(got.landing_gear, data.landing_gear);
        assert_eq!(got.train_speed, data.train_speed);
        assert_eq!(got.trailer_id, data.trailer_id);
        assert_eq!(got.position, data.position);
    }

    #[test]
    fn raw_sync_structs_roundtrip_exactly() {
        let aim = AimSyncData {
            cam_mode: 4,
            cam_front: vec3(0.0, 1.0, 0.0),
            cam_pos: vec3(10.0, 20.0, 30.0),
            aim_z: -5.5,
            cam_ext_zoom_weapon_state: 0b1010_1010,
            aspect_ratio: 200,
        };
        let mut w = BitStreamWriter::new();
        aim.write(&mut w);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 31);
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(AimSyncData::decode(&mut r).unwrap(), aim);

        let bullet = BulletSyncData {
            target_type: 1,
            target_id: 77,
            origin: vec3(1.0, 2.0, 3.0),
            target: vec3(4.0, 5.0, 6.0),
            center: vec3(7.0, 8.0, 9.0),
            weapon_id: 31,
        };
        let mut w = BitStreamWriter::new();
        bullet.write(&mut w);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 40);
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(BulletSyncData::decode(&mut r).unwrap(), bullet);

        let trailer = TrailerSyncData {
            trailer_id: 412,
            position: vec3(1.0, 2.0, 3.0),
            quaternion: Quaternion {
                x: 0.1,
                y: 0.2,
                z: 0.3,
                w: 0.4,
            },
            move_speed: vec3(0.5, 0.6, 0.7),
            turn_speed: vec3(0.8, 0.9, 1.0),
        };
        let mut w = BitStreamWriter::new();
        trailer.write(&mut w);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 54);
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(TrailerSyncData::decode(&mut r).unwrap(), trailer);

        let unocc = UnoccupiedSyncData {
            vehicle_id: 9,
            seat_id: 1,
            roll: vec3(1.0, 0.0, 0.0),
            direction: vec3(0.0, 1.0, 0.0),
            position: vec3(100.0, 200.0, 10.0),
            move_speed: vec3(0.1, 0.2, 0.3),
            turn_speed: vec3(0.4, 0.5, 0.6),
            vehicle_health: 1000.0,
        };
        let mut w = BitStreamWriter::new();
        unocc.write(&mut w);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 67);
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(UnoccupiedSyncData::decode(&mut r).unwrap(), unocc);

        let passenger = PassengerSyncData {
            vehicle_id: 411,
            seat_drive_cuff: 0b0100_0001,
            weapon_special: 0b1100_0010,
            health: 88,
            armor: 12,
            left_right_keys: 0x1111,
            up_down_keys: 0x2222,
            keys_data: 0x3333,
            position: vec3(5.0, 6.0, 7.0),
        };
        let mut w = BitStreamWriter::new();
        passenger.write(&mut w);
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 24);
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(PassengerSyncData::decode(&mut r).unwrap(), passenger);
    }
}
