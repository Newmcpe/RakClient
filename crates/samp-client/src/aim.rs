//! Native aim-sync emulation, ported from `aim_fix_updated.lua`.
//!
//! A real SA-MP client streams an aim-sync packet describing where its camera points. A bare bot
//! never sends one, which looks unnatural. This makes the client periodically (every random 5–60 s,
//! and only while standing still) send a believable aim packet: a camera placed ~2 units behind the
//! bot per its facing, jittered by a small random offset, looking back at the bot. When the server
//! repositions the bot (`SetPlayerPos`/camera RPCs) the aim is regenerated and sent promptly.

use std::time::{Duration, Instant};

use samp_proto::{AimSyncData, BitStreamWriter, Quaternion, SyncPacketId, Vector3};

/// `camExtZoom = 63` (max, 6 bits) with `weaponState = 0` (high 2 bits) — matches the reference.
const CAM_EXT_ZOOM_WEAPON_STATE: u8 = 63;
const ASPECT_RATIO: u8 = 85;
const AIM_MIN_SECS: u64 = 5;
const AIM_MAX_SECS: u64 = 60;

/// Aim-sync state machine. Construct with [`AimSync::new`], feed it position updates and server
/// repositions, and poll [`AimSync::due_packet`] each sync tick for a packet to send.
pub(crate) struct AimSync {
    rng: u64,
    last_pos: Vector3,
    aim: AimSyncData,
    next_at: Option<Instant>,
    is_regular_pos: bool,
    cam_offset: Vector3,
    bot_moved: bool,
}

impl AimSync {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            rng: seed | 1,
            last_pos: Vector3::default(),
            aim: AimSyncData::default(),
            next_at: None,
            is_regular_pos: false,
            cam_offset: Vector3::default(),
            bot_moved: false,
        }
    }

    /// Start the timer once the bot is spawned, generating an initial aim from the current pose.
    pub(crate) fn arm(
        &mut self,
        now: Instant,
        pos: Vector3,
        rotation: Quaternion,
        in_vehicle: bool,
    ) {
        self.last_pos = pos;
        self.generate(pos, rotation, in_vehicle, false);
        self.schedule(now);
    }

    /// Note the bot's current position (called as on-foot sync is built). A position change marks the
    /// bot as moving, which suppresses the next aim send.
    pub(crate) fn on_position(&mut self, pos: Vector3) {
        if pos != self.last_pos {
            self.bot_moved = true;
        }
        self.last_pos = pos;
    }

    /// The server repositioned the bot: regenerate the aim and send it on the next tick.
    pub(crate) fn on_reposition(&mut self, pos: Vector3, rotation: Quaternion, in_vehicle: bool) {
        self.last_pos = pos;
        self.generate(pos, rotation, in_vehicle, self.is_regular_pos);
        self.is_regular_pos = true;
        self.next_at = Some(Instant::now() - Duration::from_secs(1)); // due now
    }

    /// If an aim send is due (and the bot is not moving), return the packet body (`[203][AimSync]`)
    /// and reschedule. Moving consumes the move flag and skips this cycle.
    pub(crate) fn due_packet(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.bot_moved {
            self.bot_moved = false;
            return None;
        }
        match self.next_at {
            Some(at) if at <= now => {
                self.schedule(now);
                let mut w = BitStreamWriter::new();
                self.aim.encode(&mut w);
                let body = w.into_bytes();
                let mut packet = Vec::with_capacity(body.len() + 1);
                packet.push(SyncPacketId::AimSync as u8);
                packet.extend_from_slice(&body);
                Some(packet)
            }
            _ => None,
        }
    }

    /// Reset on disconnect.
    pub(crate) fn reset(&mut self) {
        self.is_regular_pos = false;
        self.bot_moved = false;
        self.cam_offset = Vector3::default();
        self.next_at = None;
    }

    fn schedule(&mut self, now: Instant) {
        let secs = AIM_MIN_SECS + self.next_u64() % (AIM_MAX_SECS - AIM_MIN_SECS + 1);
        self.next_at = Some(now + Duration::from_secs(secs));
    }

    /// `genAimSyncInfo`: camera ~2 units behind the bot per its yaw, plus a random offset, looking
    /// back at the bot with small jitter.
    fn generate(&mut self, pos: Vector3, rotation: Quaternion, in_vehicle: bool, is_static: bool) {
        if !is_static {
            self.cam_offset = self.random_vector(0.1, 1.5);
        }
        let angle = -yaw_degrees(rotation) * std::f32::consts::PI / 180.0;
        let cam = Vector3 {
            x: pos.x - 2.0 * angle.sin() + self.cam_offset.x,
            y: pos.y - 2.0 * angle.cos() + self.cam_offset.y,
            z: pos.z + 1.0 + self.cam_offset.z,
        };
        let cam_front = if is_static {
            self.aim.cam_front
        } else {
            let jitter = self.random_vector(0.1, 0.3);
            normalize(Vector3 {
                x: pos.x - cam.x + jitter.x,
                y: pos.y - cam.y + jitter.y,
                z: pos.z - cam.z + jitter.z,
            })
        };
        self.aim = AimSyncData {
            cam_mode: if in_vehicle { 18 } else { 4 },
            cam_front,
            cam_pos: cam,
            aim_z: 0.0,
            cam_ext_zoom_weapon_state: CAM_EXT_ZOOM_WEAPON_STATE,
            aspect_ratio: ASPECT_RATIO,
        };
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64.
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn random_vector(&mut self, lower: f32, upper: f32) -> Vector3 {
        Vector3 {
            x: self.random_component(lower, upper),
            y: self.random_component(lower, upper),
            z: self.random_component(lower, upper),
        }
    }

    fn random_component(&mut self, lower: f32, upper: f32) -> f32 {
        let magnitude = lower + self.unit_f32() * (upper - lower);
        if self.next_u64() & 1 == 0 {
            -magnitude
        } else {
            magnitude
        }
    }
}

/// Yaw (Z rotation) of the on-foot quaternion, in degrees.
fn yaw_degrees(q: Quaternion) -> f32 {
    (2.0 * (q.w * q.z + q.x * q.y))
        .atan2(1.0 - 2.0 * (q.y * q.y + q.z * q.z))
        .to_degrees()
}

fn normalize(v: Vector3) -> Vector3 {
    let len = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
    if len == 0.0 {
        v
    } else {
        Vector3 {
            x: v.x / len,
            y: v.y / len,
            z: v.z / len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_when_due_and_still_then_reschedules() {
        let mut aim = AimSync::new(0xDEAD_BEEF);
        let t0 = Instant::now();
        aim.arm(t0, Vector3::default(), Quaternion::default(), false);
        // Not due yet (scheduled 5–60s out).
        assert!(aim.due_packet(t0).is_none());
        // Far in the future → due. Packet is [203] + 31-byte AimSyncData.
        let later = t0 + Duration::from_secs(120);
        let packet = aim.due_packet(later).expect("aim due");
        assert_eq!(packet[0], SyncPacketId::AimSync as u8);
        assert_eq!(packet.len(), 1 + 31);
        // Immediately after, not due again.
        assert!(aim.due_packet(later).is_none());
    }

    #[test]
    fn moving_suppresses_one_cycle() {
        let mut aim = AimSync::new(1);
        let t0 = Instant::now();
        aim.arm(t0, Vector3::default(), Quaternion::default(), false);
        aim.on_position(Vector3 {
            x: 5.0,
            y: 0.0,
            z: 0.0,
        }); // moved
        let later = t0 + Duration::from_secs(120);
        assert!(aim.due_packet(later).is_none(), "suppressed while moving");
        // Next cycle, standing still → sends.
        assert!(aim.due_packet(later).is_some());
    }

    #[test]
    fn reposition_makes_it_due() {
        let mut aim = AimSync::new(2);
        let t0 = Instant::now();
        aim.arm(t0, Vector3::default(), Quaternion::default(), false);
        aim.on_reposition(
            Vector3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            Quaternion::default(),
            false,
        );
        assert!(aim.due_packet(t0).is_some(), "due right after reposition");
    }
}
