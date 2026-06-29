//! Standard client behaviours a real SA-MP client always provides and a bare bot otherwise omits:
//! answering the server's `ClientCheck` queries, streaming a weapon inventory, reporting a plausible
//! aim/camera and periodic score-ping activity, and keeping vehicle ownership consistent. Without
//! them a server treats the connection as malformed and drops it. Ported from a community MoonLoader
//! client script (its auth/join handling is excluded; that lives in the Luau Arizona launcher).
//!
//! Only the timing/bookkeeping that is not player state lives here; the weapon inventory, aim and
//! vehicle live in [`LocalPlayer`]. The driver consults this at the incoming-RPC and outgoing-sync
//! seams and sends whatever it returns.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use samp_proto::events::{self, FieldValue as F};
use samp_proto::{encode_weapons_update, OutboundMsg, SyncPacketId, RELIABILITY_RELIABLE_ORDERED};

use crate::state::{AimData, LocalPlayer};

const RPC_RESET_WEAPONS: u8 = 21;
const RPC_GIVE_WEAPON: u8 = 22;
const RPC_SET_ARMED_WEAPON: u8 = 67;
const RPC_CLIENT_CHECK: u8 = 103;
const RPC_SET_WEAPON_AMMO: u8 = 145;

/// The on-ground player-state address the server expects echoed in the `ClientCheck` `0x2` answer.
const ADR_ON_GROUND: i32 = 268_435_970;

pub(crate) struct ClientEmulation {
    rng: u64,
    started: Instant,
    uptime_base: f32,
    z_cam: f32,
    next_weapons_update: Instant,
    ping_next_update: Instant,
    ping_key_action: bool,
    model_hashes: HashMap<u32, u8>,
    col_hashes: HashMap<u32, u8>,
}

impl ClientEmulation {
    pub(crate) fn new(seed: u64, now: Instant) -> Self {
        let mut rng = seed | 1;
        let uptime_base = random_float(&mut rng, 1800.0, 43200.0);
        let z_cam = random_signed(&mut rng, 0.1, 0.2);
        Self {
            rng,
            started: now,
            uptime_base,
            z_cam,
            next_weapons_update: now,
            ping_next_update: now + secs(&mut rng, 10, 120),
            ping_key_action: false,
            model_hashes: HashMap::new(),
            col_hashes: HashMap::new(),
        }
    }

    pub(crate) fn spoof_aim(&mut self, aim: &mut AimData, lp: &LocalPlayer) {
        aim.cam_front.z = self.z_cam;
        aim.cam_pos.z += random_float(&mut self.rng, 0.35, 0.6);
        aim.weapon_state = lp.weapons.current_state();
    }

    /// Returns the weapon to report in the on-foot sync and the keys, occasionally bumped by one to
    /// mimic a real client's periodic score-ping key activity.
    pub(crate) fn adjust_on_foot(
        &mut self,
        lp: &LocalPlayer,
        keys: u16,
        now: Instant,
    ) -> (u8, u16) {
        if now >= self.ping_next_update {
            self.ping_key_action = true;
            self.ping_next_update = now + secs(&mut self.rng, 10, 120);
        }
        let keys = if self.ping_key_action {
            self.ping_key_action = false;
            keys.wrapping_add(1)
        } else {
            keys
        };
        (lp.weapons.current, keys)
    }

    pub(crate) fn due_weapons_update(
        &mut self,
        lp: &LocalPlayer,
        now: Instant,
    ) -> Option<OutboundMsg> {
        if now < self.next_weapons_update {
            return None;
        }
        self.next_weapons_update = now + millis(&mut self.rng, 200, 500);
        Some(weapons_update_msg(lp))
    }

    /// React to a server RPC, returning packets/RPCs to send back. Position RPCs
    /// (`SetPlayerPos`/`FindZ`/`InterpolateCamera`) are handled by the driver's aim-follow path.
    pub(crate) fn on_incoming_rpc(
        &mut self,
        lp: &mut LocalPlayer,
        id: u8,
        payload: &[u8],
    ) -> Vec<OutboundMsg> {
        match id {
            RPC_CLIENT_CHECK => self.answer_client_check(lp, payload).into_iter().collect(),
            RPC_GIVE_WEAPON => {
                let Some((weapon, ammo)) = decode_two_i32(id, payload) else {
                    return Vec::new();
                };
                if lp
                    .weapons
                    .give(weapon as u8, ammo.clamp(0, u16::MAX as i32) as u16)
                {
                    vec![weapons_update_msg(lp)]
                } else {
                    Vec::new()
                }
            }
            RPC_SET_ARMED_WEAPON => {
                if let Some(F::I32(weapon)) = first_field(id, payload) {
                    lp.weapons.set_armed(weapon as u8);
                }
                Vec::new()
            }
            RPC_SET_WEAPON_AMMO => {
                let values = match events::decode_incoming(id, payload) {
                    Some(Ok((_, values))) => values,
                    _ => return Vec::new(),
                };
                if let (Some(F::U8(weapon)), Some(F::U16(ammo))) = (values.first(), values.get(1)) {
                    if lp.weapons.set_ammo(*weapon, *ammo) {
                        return vec![weapons_update_msg(lp)];
                    }
                }
                Vec::new()
            }
            RPC_RESET_WEAPONS => {
                lp.weapons.reset();
                vec![weapons_update_msg(lp)]
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn is_vehicle_hijack(&self, lp: &LocalPlayer, payload: &[u8]) -> bool {
        let Some(vehicle) = lp.vehicle else {
            return false;
        };
        let body = payload.get(1..).unwrap_or_default();
        let mut r = samp_proto::BitStreamReader::new(body);
        matches!(
            samp_proto::VehicleSyncData::decode(&mut r),
            Ok(data) if data.vehicle_id == vehicle.id
        )
    }

    pub(crate) fn reset(&mut self, lp: &mut LocalPlayer) {
        lp.weapons.reset();
        self.ping_key_action = false;
        self.next_weapons_update = self.started;
    }

    fn answer_client_check(&mut self, lp: &LocalPlayer, payload: &[u8]) -> Option<OutboundMsg> {
        let values = match events::decode_incoming(RPC_CLIENT_CHECK, payload) {
            Some(Ok((_, values))) => values,
            _ => return None,
        };
        let (F::U8(request_type), Some(F::I32(subject))) = (values.first()?, values.get(1)) else {
            return None;
        };
        // 0x2 player state, 0x48 client uptime, 0x46/0x47 model/collision checksums.
        let (result1, result2): (i32, u8) = match *request_type {
            0x2 => (ADR_ON_GROUND, if lp.in_vehicle() { 2 } else { 1 }),
            0x48 => {
                let uptime = self.uptime_base + self.started.elapsed().as_secs_f32();
                (uptime as i64 as i32, 0)
            }
            0x46 => (
                *subject,
                memo_hash(&mut self.model_hashes, &mut self.rng, *subject as u32),
            ),
            0x47 => (
                *subject,
                memo_hash(&mut self.col_hashes, &mut self.rng, *subject as u32),
            ),
            other => {
                tracing::debug!(request_type = other, "unhandled ClientCheck type");
                return None;
            }
        };
        let body = events::encode_outgoing(
            RPC_CLIENT_CHECK,
            &[F::U8(*request_type), F::I32(result1), F::U8(result2)],
        )?
        .ok()?;
        Some(OutboundMsg::Rpc {
            id: RPC_CLIENT_CHECK,
            payload: body,
        })
    }
}

fn weapons_update_msg(lp: &LocalPlayer) -> OutboundMsg {
    let body = encode_weapons_update(&lp.weapons.slots);
    let mut data = Vec::with_capacity(body.len() + 1);
    data.push(SyncPacketId::WeaponsUpdate as u8);
    data.extend_from_slice(&body);
    OutboundMsg::Packet {
        data,
        reliability: RELIABILITY_RELIABLE_ORDERED,
        channel: 0,
    }
}

fn first_field(id: u8, payload: &[u8]) -> Option<F> {
    match events::decode_incoming(id, payload) {
        Some(Ok((_, values))) => values.into_iter().next(),
        _ => None,
    }
}

fn decode_two_i32(id: u8, payload: &[u8]) -> Option<(i32, i32)> {
    match events::decode_incoming(id, payload) {
        Some(Ok((_, values))) => match (values.first(), values.get(1)) {
            (Some(F::I32(a)), Some(F::I32(b))) => Some((*a, *b)),
            _ => None,
        },
        _ => None,
    }
}

// SplitMix64, matching `aim.rs`.
fn next_u64(rng: &mut u64) -> u64 {
    *rng = rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *rng;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn unit_f32(rng: &mut u64) -> f32 {
    (next_u64(rng) >> 40) as f32 / (1u64 << 24) as f32
}

fn random_float(rng: &mut u64, lower: f32, upper: f32) -> f32 {
    lower + unit_f32(rng) * (upper - lower)
}

fn random_signed(rng: &mut u64, lower: f32, upper: f32) -> f32 {
    let magnitude = random_float(rng, lower, upper);
    if next_u64(rng) & 1 == 0 {
        -magnitude
    } else {
        magnitude
    }
}

fn rand_hash(rng: &mut u64) -> u8 {
    1 + (next_u64(rng) % 255) as u8
}

/// Memoize a random checksum per address so the server gets a consistent answer each time it asks.
fn memo_hash(map: &mut HashMap<u32, u8>, rng: &mut u64, key: u32) -> u8 {
    *map.entry(key).or_insert_with(|| rand_hash(rng))
}

fn secs(rng: &mut u64, lo: u64, hi: u64) -> Duration {
    Duration::from_secs(lo + next_u64(rng) % (hi - lo + 1))
}

fn millis(rng: &mut u64, lo: u64, hi: u64) -> Duration {
    Duration::from_millis(lo + next_u64(rng) % (hi - lo + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn lp() -> LocalPlayer {
        LocalPlayer::new(
            "Bot".into(),
            "127.0.0.1:7777".parse::<SocketAddr>().unwrap(),
        )
    }

    fn client_check_body(request_type: u8, subject: i32) -> Vec<u8> {
        events::encode_incoming(
            RPC_CLIENT_CHECK,
            &[F::U8(request_type), F::I32(subject), F::U16(0), F::U16(0)],
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn client_check_player_state_reports_on_ground_and_on_foot() {
        let mut e = ClientEmulation::new(1, Instant::now());
        let mut player = lp();
        let out = e.on_incoming_rpc(&mut player, RPC_CLIENT_CHECK, &client_check_body(0x2, 0));
        assert_eq!(out.len(), 1);
        let OutboundMsg::Rpc { id, payload } = &out[0] else {
            panic!("expected rpc")
        };
        assert_eq!(*id, RPC_CLIENT_CHECK);
        assert_eq!(payload[0], 0x2);
        assert_eq!(
            i32::from_le_bytes(payload[1..5].try_into().unwrap()),
            ADR_ON_GROUND
        );
        assert_eq!(payload[5], 1);
    }

    #[test]
    fn client_check_model_hash_is_memoized() {
        let mut e = ClientEmulation::new(7, Instant::now());
        let mut player = lp();
        let first = e.on_incoming_rpc(&mut player, RPC_CLIENT_CHECK, &client_check_body(0x46, 999));
        let again = e.on_incoming_rpc(&mut player, RPC_CLIENT_CHECK, &client_check_body(0x46, 999));
        let hash = |o: &[OutboundMsg]| match &o[0] {
            OutboundMsg::Rpc { payload, .. } => payload[5],
            _ => panic!(),
        };
        assert_eq!(hash(&first), hash(&again));
    }

    #[test]
    fn give_weapon_emits_weapons_update_and_tracks_inventory() {
        let mut e = ClientEmulation::new(2, Instant::now());
        let mut player = lp();
        let body = events::encode_incoming(RPC_GIVE_WEAPON, &[F::I32(24), F::I32(50)])
            .unwrap()
            .unwrap();
        let out = e.on_incoming_rpc(&mut player, RPC_GIVE_WEAPON, &body);
        assert_eq!(out.len(), 1);
        assert!(
            matches!(&out[0], OutboundMsg::Packet { data, .. } if data[0] == SyncPacketId::WeaponsUpdate as u8)
        );
        assert_eq!(player.weapons.current, 24);
        assert_eq!(player.weapons.slots[2].ammo, 50);
    }

    #[test]
    fn weapons_update_is_rate_limited() {
        let now = Instant::now();
        let mut e = ClientEmulation::new(3, now);
        let player = lp();
        assert!(e.due_weapons_update(&player, now).is_some());
        assert!(e.due_weapons_update(&player, now).is_none());
        assert!(e
            .due_weapons_update(&player, now + Duration::from_secs(1))
            .is_some());
    }

    #[test]
    fn vehicle_hijack_only_when_in_that_vehicle() {
        let e = ClientEmulation::new(4, Instant::now());
        let mut player = lp();
        let body = sample_vehicle_sync(77);
        assert!(!e.is_vehicle_hijack(&player, &body));
        player.vehicle = Some(crate::state::InVehicleData {
            id: 77,
            ..Default::default()
        });
        assert!(e.is_vehicle_hijack(&player, &body));
        assert!(!e.is_vehicle_hijack(&player, &sample_vehicle_sync(88)));
    }

    fn sample_vehicle_sync(vehicle_id: u16) -> Vec<u8> {
        use samp_proto::Encode;
        samp_proto::VehicleSyncData {
            vehicle_id,
            ..Default::default()
        }
        .to_packet()
    }
}
