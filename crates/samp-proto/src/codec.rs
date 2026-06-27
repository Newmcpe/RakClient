//! RPC payloads and the on-foot sync body, ported from the 0.3.7 client.
//!
//! Every encoder produces the RPC/packet *body*; the id byte is prepended by the transport.

use crate::bitstream::{BitStreamReader, BitStreamWriter};
use crate::{ClassId, PlayerId, Quaternion, Result, Skin, Vector3, WeaponId};

/// Outgoing `RPC_ClientJoin` (25). `challenge_response` is `server_cookie ^ CHALLENGE_XOR`.
///
/// When `duplicate_challenge_response` is set, `encode_client_join` appends a trailing copy of
/// `challenge_response` as a 7th field — the Arizona 0.3.7-R3 client variant (verified in
/// `samp.dll:0x1000AA20`, where the join sender writes `challengeResponse` twice). Vanilla SA-MP
/// reads only the first six fields and ignores the trailing bytes, so this is safe to leave on; set
/// it `false` for a strict-vanilla join.
#[derive(Debug, Clone)]
pub struct ClientJoin<'a> {
    pub version: u32,
    pub modded: bool,
    pub nick: &'a str,
    pub challenge_response: u32,
    pub auth: &'a str,
    pub client_version: &'a str,
    pub duplicate_challenge_response: bool,
}

/// Build the `RPC_ClientJoin` body. Field order/sizes verified against
/// `Net_OnConnAccepted_SendClientJoin` (0x4572C0):
/// `version:u32, modded:u8, nick:str8, challenge_response:u32, auth:str8, client_version:str8`.
/// The Arizona variant appends a duplicated `challenge_response:u32` as a 7th field.
pub fn encode_client_join(join: &ClientJoin<'_>) -> Vec<u8> {
    let mut w = BitStreamWriter::new();
    w.write_u32(join.version);
    w.write_u8(join.modded as u8);
    w.write_str8(join.nick);
    w.write_u32(join.challenge_response);
    w.write_str8(join.auth);
    w.write_str8(join.client_version);
    if join.duplicate_challenge_response {
        w.write_u32(join.challenge_response);
    }
    w.into_bytes()
}

// `RPC_InitGame` (139) bit layout (RPC_InitGame @0x458F90). The original handler consumes many
// server-settings fields the client mostly discards; we replicate the exact cursor advance and
// extract only the two fields this crate models.
// TODO(verify): the skipped server-settings block is reproduced bit-for-bit from the binary's
// read sequence but the semantic meaning of individual skipped fields is not modelled here.
const INIT_GAME_BITS_BEFORE_PLAYER_ID: usize = 104;
const INIT_GAME_BITS_BETWEEN_PLAYER_ID_AND_HOSTNAME: usize = 275;

/// Incoming `RPC_InitGame` (139). Only the fields the client needs are modelled.
#[derive(Debug, Clone, Default)]
pub struct InitGame {
    pub local_player_id: PlayerId,
    pub host_name: String,
}

/// Decode `RPC_InitGame`, extracting our assigned player id and the server host name.
///
/// ```
/// use samp_proto::decode_init_game;
/// assert!(decode_init_game(&[]).is_err());
/// ```
pub fn decode_init_game(payload: &[u8]) -> Result<InitGame> {
    let mut r = BitStreamReader::new(payload);
    r.skip_bits(INIT_GAME_BITS_BEFORE_PLAYER_ID)?;
    let local_player_id = PlayerId(r.read_u16()?);
    r.skip_bits(INIT_GAME_BITS_BETWEEN_PLAYER_ID_AND_HOSTNAME)?;
    let host_name = r.read_str8()?;
    Ok(InitGame {
        local_player_id,
        host_name,
    })
}

/// Outgoing `RPC_RequestClass` (128) — body is the class id as a 32-bit integer
/// (Net_SendRequestClass @0x455AC0).
pub fn encode_request_class(class: ClassId) -> Vec<u8> {
    let mut w = BitStreamWriter::new();
    w.write_i32(class.0);
    w.into_bytes()
}

// Offsets within the 46-byte `PLAYER_SPAWN_INFO` blob (read by sub_401F80; field offsets confirmed
// against Net_Spawn @0x455BB0 which reads skin@+1, position xy@+6, position z@+14).
const SPAWN_INFO_LEN: usize = 46;
const SPAWN_INFO_SKIN_OFFSET: usize = 1;
const SPAWN_INFO_POS_OFFSET: usize = 6;

/// Incoming `RPC_RequestClass` response (128) — server confirms class selection + spawn info.
#[derive(Debug, Clone, Default)]
pub struct RequestClassResponse {
    pub allowed: bool,
    pub spawn_position: Vector3,
    pub skin: Skin,
}

/// Decode the class-selection response. `allow != 0` is followed by a 46-byte spawn-info blob.
///
/// ```
/// use samp_proto::decode_request_class_response;
/// // allow byte says "yes" but the spawn-info blob is missing -> error, not panic.
/// assert!(decode_request_class_response(&[1]).is_err());
/// ```
pub fn decode_request_class_response(payload: &[u8]) -> Result<RequestClassResponse> {
    let mut r = BitStreamReader::new(payload);
    let allow = r.read_u8()?;
    if allow == 0 {
        return Ok(RequestClassResponse::default());
    }
    let info = r.read_bytes(SPAWN_INFO_LEN)?;
    let skin = Skin(u16::from_le_bytes([
        info[SPAWN_INFO_SKIN_OFFSET],
        info[SPAWN_INFO_SKIN_OFFSET + 1],
    ]));
    let spawn_position = Vector3 {
        x: read_f32_at(&info, SPAWN_INFO_POS_OFFSET),
        y: read_f32_at(&info, SPAWN_INFO_POS_OFFSET + 4),
        z: read_f32_at(&info, SPAWN_INFO_POS_OFFSET + 8),
    };
    Ok(RequestClassResponse {
        allowed: true,
        spawn_position,
        skin,
    })
}

fn read_f32_at(buf: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

/// Outgoing `RPC_RequestSpawn` (129) — empty body (Net_SendRequestSpawn @0x455D50).
pub fn encode_request_spawn() -> Vec<u8> {
    Vec::new()
}

/// Incoming `RPC_RequestSpawn` response (129). `allow == 2`, or `allow != 0` while spawn was
/// requested, means the server is letting us spawn.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestSpawnResponse {
    pub allow: u8,
}

/// Decode the spawn response (single allow byte).
///
/// ```
/// use samp_proto::decode_request_spawn_response;
/// assert!(decode_request_spawn_response(&[]).is_err());
/// ```
pub fn decode_request_spawn_response(payload: &[u8]) -> Result<RequestSpawnResponse> {
    let mut r = BitStreamReader::new(payload);
    Ok(RequestSpawnResponse {
        allow: r.read_u8()?,
    })
}

/// Outgoing `RPC_Spawn` (52) — empty body (Net_Spawn @0x455BB0).
pub fn encode_spawn() -> Vec<u8> {
    Vec::new()
}

/// Outgoing on-foot sync body (`ID_PLAYER_SYNC` = 207).
#[derive(Debug, Clone, Copy, Default)]
pub struct OnFootSync {
    pub keys: u16,
    pub position: Vector3,
    pub quaternion: Quaternion,
    pub health: u8,
    pub armour: u8,
    pub weapon: WeaponId,
    pub special_action: u8,
}

/// On-foot sync body length in bytes (544 bits), per the on-foot branch of Net_SendInGameSync
/// (0x455210).
pub const ON_FOOT_SYNC_LEN: usize = 68;

/// Encode the 544-bit on-foot sync body. Layout (byte offsets) ported from Net_SendInGameSync's
/// on-foot branch:
/// `lrAnalog:u16@0, udAnalog:u16@2, keys:u16@4, position:3xf32@6, quaternion:4xf32@18,
/// health:u8@34, armour:u8@35, weapon:u8@36, special_action:u8@37, moveSpeed:3xf32@38,
/// surfOffset:3xf32@50, surfVehicle:u16@62, animIndex:u16@64, animFlags:u16@66`.
///
/// The id byte is prepended by the caller/transport.
pub fn encode_on_foot_sync(sync: &OnFootSync) -> Vec<u8> {
    let mut w = BitStreamWriter::new();
    // Left/right + up/down analog steering: the original sender zeroes these.
    w.write_u16(0);
    w.write_u16(0);
    w.write_u16(sync.keys);
    w.write_f32(sync.position.x);
    w.write_f32(sync.position.y);
    w.write_f32(sync.position.z);
    // TODO(verify): quaternion component order — modelled as (x, y, z, w) to match `Quaternion`;
    // the binary copies 16 raw bytes from the rotation state.
    w.write_f32(sync.quaternion.x);
    w.write_f32(sync.quaternion.y);
    w.write_f32(sync.quaternion.z);
    w.write_f32(sync.quaternion.w);
    w.write_u8(sync.health);
    w.write_u8(sync.armour);
    w.write_u8(sync.weapon.0);
    w.write_u8(sync.special_action);
    // Move speed (x, y, z): unmodelled, zeroed.
    w.write_f32(0.0);
    w.write_f32(0.0);
    w.write_f32(0.0);
    // Surfing offset (x, y, z): unmodelled, zeroed.
    w.write_f32(0.0);
    w.write_f32(0.0);
    w.write_f32(0.0);
    w.write_u16(0); // surfing vehicle id
                    // TODO(verify): the binary writes a fixed animation pair (animIndex=0x04A5, animFlags=0x8004);
                    // we send a neutral 0/0 since OnFootSync does not model animation state.
    w.write_u16(0); // animation index
    w.write_u16(0); // animation flags
    w.into_bytes()
}

/// A server→client coloured chat/system line (`RPC_ClientMessage` = 93). `text` is raw bytes in the
/// server's encoding (cp1251 on Russian SA-MP servers); decode with [`crate::decode_cp1251`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerMessage {
    /// RGBA colour as sent on the wire (`0xRRGGBBAA`).
    pub color: u32,
    pub text: Vec<u8>,
}

/// A server→client player chat broadcast (`RPC_Chat` = 101). `text` is raw bytes (see
/// [`crate::decode_cp1251`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub player_id: PlayerId,
    pub text: Vec<u8>,
}

/// Decode `RPC_ClientMessage` (93): `[u32 colour LE][u32 len LE][text]`. Verified against Arizona
/// Bumble Bee live (`ffffffff 01000000 20` ⇒ white, len 1, `" "`).
///
/// ```
/// use samp_proto::decode_client_message;
/// let msg = decode_client_message(&[0xff,0xff,0xff,0xff, 0x02,0,0,0, b'h', b'i']).unwrap();
/// assert_eq!(msg.color, 0xffff_ffff);
/// assert_eq!(msg.text, b"hi");
/// ```
pub fn decode_client_message(payload: &[u8]) -> Result<ServerMessage> {
    let mut r = BitStreamReader::new(payload);
    let color = r.read_u32()?;
    let len = r.read_u32()? as usize;
    let text = r.read_bytes(len)?;
    Ok(ServerMessage { color, text })
}

/// Decode a server→client `RPC_Chat` (101) broadcast: `[u16 playerId LE][u8 len][text]`.
///
/// ```
/// use samp_proto::decode_player_chat;
/// let msg = decode_player_chat(&[0x05, 0x00, 0x02, b'y', b'o']).unwrap();
/// assert_eq!(msg.player_id.0, 5);
/// assert_eq!(msg.text, b"yo");
/// ```
pub fn decode_player_chat(payload: &[u8]) -> Result<ChatMessage> {
    let mut r = BitStreamReader::new(payload);
    let player_id = PlayerId(r.read_u16()?);
    let len = r.read_u8()? as usize;
    let text = r.read_bytes(len)?;
    Ok(ChatMessage { player_id, text })
}

/// Encode an outgoing `RPC_Chat` (101) — what the client sends when the local player types in the
/// chat bar: `[u8 len][text]`. `text` is raw bytes in the server's encoding; the length is a single
/// byte, so callers must keep messages ≤ 255 bytes (longer input is truncated).
pub fn encode_chat(text: &[u8]) -> Vec<u8> {
    let len = text.len().min(u8::MAX as usize);
    let mut w = BitStreamWriter::new();
    w.write_u8(len as u8);
    w.write_bytes(&text[..len]);
    w.into_bytes()
}

/// Arizona custom-packet id (`0xDC`). These are raw packets (first byte = this id), sent via the
/// reliable-ordered path on ordering channel 2 — not RPCs. Sub-id follows as a second byte. The
/// whole family is the Arizona client's post-join "validation"/CEF handshake; faithfully ported
/// from the working MoonLoader addon (sub-ids 18/20/38/50/140), it carries display geometry, a
/// focus flag, a random client id and a *fabricated* game path with a random `-userId` — benign
/// filler the server requires, not an authenticated credential.
pub const AZ_PACKET_ID: u8 = 220;

/// CEF/Svelte app message (`220/18`): `[220][18][u16 len LE][text][u32 0][u16 0]`. Used for
/// `onSvelteAppInit` and `onSvelteAppVersion|…` on join.
pub fn encode_az_cef_message(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 10);
    out.push(AZ_PACKET_ID);
    out.push(18);
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Display resolution (`220/20`): `[220][20][u32 width LE][u32 height LE]`. The addon sends
/// `128,7,0,0,56,4,0,0` ⇒ 1920×1080.
pub fn encode_az_resolution(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.push(AZ_PACKET_ID);
    out.push(20);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out
}

/// Window focus flag (`220/50`): `[220][50][u8]`.
pub fn encode_az_focus(focused: bool) -> Vec<u8> {
    vec![AZ_PACKET_ID, 50, focused as u8]
}

/// Client id (`220/38`): `[220][38][ascii bytes]` — raw, no length prefix (the addon writes each
/// hex char as a byte). `id` is a 64-char hex string.
pub fn encode_az_client_id(id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len() + 2);
    out.push(AZ_PACKET_ID);
    out.push(38);
    out.extend_from_slice(id.as_bytes());
    out
}

/// Game-path packet (`220/140`): `[220][140][u32 len LE][path][u8 0]`. `path` is the fabricated
/// command line from [`build_az_game_path`].
pub fn encode_az_game_path(path: &str) -> Vec<u8> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 7);
    out.push(AZ_PACKET_ID);
    out.push(140);
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    out.push(0);
    out
}

/// Build the fabricated game command line the addon sends in `220/140`, with the bot nick and a
/// throwaway `-userId`. The `-userId` is *not* an authenticated token — the reference addon fills
/// it with [`generate_user_id`] (random hex), so any 64-hex value is accepted.
pub fn build_az_game_path(nick: &str, user_id: &str) -> String {
    format!(
        "\"gta_sa.exe\"  -c -h  -p 7777 -n {nick} -mem 2048 -window -x -widescreen -graphics \
         -enable_grass -arizona -userId {user_id} -cdn 0,0,0 -referrer"
    )
}

/// Generate a random 64-character lowercase-hex id (matches the addon's `getUniqueUserId`). Used
/// for both the `-userId` field and, optionally, the `220/38` client id.
pub fn generate_user_id() -> String {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x1234_5678_9ABC_DEF0);
    generate_user_id_seeded(seed)
}

/// Deterministic variant of [`generate_user_id`] for reproducible tests.
pub fn generate_user_id_seeded(seed: u64) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut state = seed;
    let mut id = String::with_capacity(64);
    for _ in 0..64 {
        let nibble = (next_rand(&mut state) % 16) as usize;
        id.push(HEX[nibble] as char);
    }
    id
}

/// A server→client `ShowDialog` (61). `title`/`button1`/`button2` are raw bytes (cp1251). The body
/// text (compressed on the wire) is not decoded — only the structural fields the client needs to
/// respond are returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowDialog {
    pub dialog_id: u16,
    pub style: u8,
    pub title: Vec<u8>,
    pub button1: Vec<u8>,
    pub button2: Vec<u8>,
}

/// Decode the structural head of a `ShowDialog` (61): `[u16 dialogId][u8 style][str8 title]
/// [str8 button1][str8 button2]…`. The trailing body string is left undecoded.
pub fn decode_show_dialog(payload: &[u8]) -> Result<ShowDialog> {
    let mut r = BitStreamReader::new(payload);
    let dialog_id = r.read_u16()?;
    let style = r.read_u8()?;
    let title = read_str8_bytes(&mut r)?;
    let button1 = read_str8_bytes(&mut r)?;
    let button2 = read_str8_bytes(&mut r)?;
    Ok(ShowDialog {
        dialog_id,
        style,
        title,
        button1,
        button2,
    })
}

fn read_str8_bytes(r: &mut BitStreamReader) -> Result<Vec<u8>> {
    let len = r.read_u8()? as usize;
    r.read_bytes(len)
}

/// Encode an outgoing `RPC_DialogResponse` (62): `[u16 dialogId][u8 button][u16 listItem][u8 len]
/// [text]`. `button` is `1` for the positive/left button (login/OK), `0` for the right/cancel;
/// `list_item` is the selected row (`0xFFFF` if none); `input` is the text-box content (e.g. the
/// account password), truncated to 255 bytes.
pub fn encode_dialog_response(dialog_id: u16, button: u8, list_item: u16, input: &[u8]) -> Vec<u8> {
    let len = input.len().min(u8::MAX as usize);
    let mut w = BitStreamWriter::new();
    w.write_u16(dialog_id);
    w.write_u8(button);
    w.write_u16(list_item);
    w.write_u8(len as u8);
    w.write_bytes(&input[..len]);
    w.into_bytes()
}

/// Best-effort gpci/auth token (`Key`). open.mp accepts any token whose value is divisible by
/// 1001; the exact bytes of the original client's `rand()` stream are neither reproducible nor
/// required, and callers may override.
pub fn generate_gpci() -> String {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x1234_5678_9ABC_DEF0);
    generate_gpci_seeded(seed)
}

/// Deterministic variant of [`generate_gpci`] for reproducible tests: the same `seed` always
/// yields the same token.
///
/// open.mp validates the token (`Key`) by parsing it as a base-16 integer and requiring it to be
/// divisible by 1001 (`legacy_network_impl.cpp`), so the token is `hex(random_96_bit * 1001)`.
pub fn generate_gpci_seeded(seed: u64) -> String {
    let mut state = seed;
    let hi = u128::from(next_rand(&mut state));
    let lo = u128::from(next_rand(&mut state));
    let random96 = ((hi << 64) | lo) & ((1u128 << 96) - 1);
    let token = random96.wrapping_mul(1001).max(1001);
    format!("{token:X}")
}

/// SplitMix64 — a tiny self-contained PRNG so the token builder needs no external rng dependency.
fn next_rand(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn u32_roundtrip(v: u32) {
            let mut w = BitStreamWriter::new();
            w.write_u32(v);
            let bytes = w.into_bytes();
            let mut r = BitStreamReader::new(&bytes);
            prop_assert_eq!(r.read_u32().unwrap(), v);
        }

        #[test]
        fn unaligned_mixed_roundtrip(lead in 0u8..8, v: u32, tail: u16) {
            let mut w = BitStreamWriter::new();
            for _ in 0..lead {
                w.write_bit(true);
            }
            w.write_u32(v);
            w.write_u16(tail);
            let bytes = w.into_bytes();

            let mut r = BitStreamReader::new(&bytes);
            for _ in 0..lead {
                prop_assert!(r.read_bit().unwrap());
            }
            prop_assert_eq!(r.read_u32().unwrap(), v);
            prop_assert_eq!(r.read_u16().unwrap(), tail);
        }

        #[test]
        fn bytes_roundtrip(data: Vec<u8>) {
            let mut w = BitStreamWriter::new();
            w.write_bytes(&data);
            let bytes = w.into_bytes();
            let mut r = BitStreamReader::new(&bytes);
            prop_assert_eq!(r.read_bytes(data.len()).unwrap(), data);
        }
    }
}
