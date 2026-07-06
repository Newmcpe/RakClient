//! RPC payloads and the on-foot sync body, ported from the 0.3.7 client.
//!
//! Each packet/RPC is a struct that knows its wire id ([`Packet::ID`]); client→server bodies
//! implement [`Encode`] and server→client bodies implement [`Decode`]. The id byte itself is
//! prepended by the transport, never by these bodies.

use crate::bitstream::{BitStreamReader, BitStreamWriter};
use crate::ids::{RpcId, SyncPacketId};
use crate::{ClassId, PlayerId, Quaternion, Result, ServerCookie, Skin, Vector3, WeaponId};

/// Every packet/RPC knows its id; the transport frames the id byte before the body.
pub trait Packet {
    /// The RPC/sync id byte that precedes this body on the wire.
    const ID: u8;
}

/// Client→server body: serializes itself (the id byte is NOT included).
pub trait Encode: Packet {
    fn encode(&self) -> Vec<u8>;

    /// The full wire packet with the id byte prepended (the sync/`transport.send` path; RPCs send
    /// the id separately via `transport.rpc`).
    fn to_packet(&self) -> Vec<u8> {
        let body = self.encode();
        let mut packet = Vec::with_capacity(body.len() + 1);
        packet.push(Self::ID);
        packet.extend_from_slice(&body);
        packet
    }
}

/// Server→client body: parses from the payload (the id byte is already stripped).
pub trait Decode: Packet + Sized {
    fn decode(payload: &[u8]) -> Result<Self>;
}

/// Outgoing `RPC_ClientJoin` (25). `challenge_response` is `server_cookie ^ CHALLENGE_XOR`.
///
/// When `duplicate_challenge_response` is set, [`ClientJoin::encode`] appends a trailing copy of
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

impl Packet for ClientJoin<'_> {
    const ID: u8 = RpcId::ClientJoin as u8;
}

impl Encode for ClientJoin<'_> {
    /// Build the `RPC_ClientJoin` body. Field order/sizes verified against
    /// `Net_OnConnAccepted_SendClientJoin` (0x4572C0):
    /// `version:u32, modded:u8, nick:str8, challenge_response:u32, auth:str8, client_version:str8`.
    /// The Arizona variant appends a duplicated `challenge_response:u32` as a 7th field.
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_u32(self.version);
        w.write_u8(self.modded as u8);
        w.write_str8(self.nick);
        w.write_u32(self.challenge_response);
        w.write_str8(self.auth);
        w.write_str8(self.client_version);
        if self.duplicate_challenge_response {
            w.write_u32(self.challenge_response);
        }
        w.into_bytes()
    }
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

impl Packet for InitGame {
    const ID: u8 = RpcId::InitGame as u8;
}

impl Decode for InitGame {
    /// Decode `RPC_InitGame`, extracting our assigned player id and the server host name.
    ///
    /// ```
    /// use samp_proto::{Decode, InitGame};
    /// assert!(InitGame::decode(&[]).is_err());
    /// ```
    fn decode(payload: &[u8]) -> Result<Self> {
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
}

impl Encode for InitGame {
    /// Re-encode a minimal valid `RPC_InitGame`: the skipped server-settings blocks are zero-filled,
    /// so `InitGame::decode(x.encode())` recovers `local_player_id` and `host_name` — the only two
    /// fields this crate models. Used by the mock server / unit tests to produce a join payload.
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_zero_bits(INIT_GAME_BITS_BEFORE_PLAYER_ID);
        w.write_u16(self.local_player_id.0);
        w.write_zero_bits(INIT_GAME_BITS_BETWEEN_PLAYER_ID_AND_HOSTNAME);
        w.write_str8(&self.host_name);
        w.into_bytes()
    }
}

/// Outgoing `RPC_RequestClass` (128) — body is the class id as a 32-bit integer
/// (Net_SendRequestClass @0x455AC0).
#[derive(Debug, Clone, Copy)]
pub struct RequestClass {
    pub class: ClassId,
}

impl Packet for RequestClass {
    const ID: u8 = RpcId::RequestClass as u8;
}

impl Encode for RequestClass {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_i32(self.class.0);
        w.into_bytes()
    }
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

impl Packet for RequestClassResponse {
    const ID: u8 = RpcId::RequestClass as u8;
}

impl Decode for RequestClassResponse {
    /// Decode the class-selection response. `allow != 0` is followed by a 46-byte spawn-info blob.
    ///
    /// ```
    /// use samp_proto::{Decode, RequestClassResponse};
    /// // allow byte says "yes" but the spawn-info blob is missing -> error, not panic.
    /// assert!(RequestClassResponse::decode(&[1]).is_err());
    /// ```
    fn decode(payload: &[u8]) -> Result<Self> {
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
}

impl Encode for RequestClassResponse {
    /// Re-encode the class-selection response (inverse of [`RequestClassResponse::decode`]): a
    /// denied response is the single `0` allow byte; an allowed one is `1` followed by the 46-byte
    /// spawn-info blob carrying `skin`/`spawn_position`. Used by the mock server / unit tests.
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        if !self.allowed {
            w.write_u8(0);
            return w.into_bytes();
        }
        w.write_u8(1);
        let mut info = [0u8; SPAWN_INFO_LEN];
        info[SPAWN_INFO_SKIN_OFFSET..SPAWN_INFO_SKIN_OFFSET + 2]
            .copy_from_slice(&self.skin.0.to_le_bytes());
        info[SPAWN_INFO_POS_OFFSET..SPAWN_INFO_POS_OFFSET + 4]
            .copy_from_slice(&self.spawn_position.x.to_le_bytes());
        info[SPAWN_INFO_POS_OFFSET + 4..SPAWN_INFO_POS_OFFSET + 8]
            .copy_from_slice(&self.spawn_position.y.to_le_bytes());
        info[SPAWN_INFO_POS_OFFSET + 8..SPAWN_INFO_POS_OFFSET + 12]
            .copy_from_slice(&self.spawn_position.z.to_le_bytes());
        w.write_bytes(&info);
        w.into_bytes()
    }
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
#[derive(Debug, Clone, Copy)]
pub struct RequestSpawn;

impl Packet for RequestSpawn {
    const ID: u8 = RpcId::RequestSpawn as u8;
}

impl Encode for RequestSpawn {
    fn encode(&self) -> Vec<u8> {
        Vec::new()
    }
}

/// Incoming `RPC_RequestSpawn` response (129). `allow == 2`, or `allow != 0` while spawn was
/// requested, means the server is letting us spawn.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestSpawnResponse {
    pub allow: u8,
}

impl Packet for RequestSpawnResponse {
    const ID: u8 = RpcId::RequestSpawn as u8;
}

impl Decode for RequestSpawnResponse {
    /// Decode the spawn response (single allow byte).
    ///
    /// ```
    /// use samp_proto::{Decode, RequestSpawnResponse};
    /// assert!(RequestSpawnResponse::decode(&[]).is_err());
    /// ```
    fn decode(payload: &[u8]) -> Result<Self> {
        let mut r = BitStreamReader::new(payload);
        Ok(RequestSpawnResponse {
            allow: r.read_u8()?,
        })
    }
}

impl Encode for RequestSpawnResponse {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_u8(self.allow);
        w.into_bytes()
    }
}

/// Outgoing `RPC_Spawn` (52) — empty body (Net_Spawn @0x455BB0).
#[derive(Debug, Clone, Copy)]
pub struct Spawn;

impl Packet for Spawn {
    const ID: u8 = RpcId::Spawn as u8;
}

impl Encode for Spawn {
    fn encode(&self) -> Vec<u8> {
        Vec::new()
    }
}

/// Outgoing `PACKET_STATS_UPDATE` (205): `[i32 money][i32 drunk]` (8 bytes). The real client sends
/// this every second while spawned (`NetGame_Process` @0x10005B10 writes the id then money and the
/// drunk level, each as a 32-bit little-endian int, `Send` UNRELIABLE on channel 0). The id byte is
/// prepended by the transport.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatsUpdate {
    pub money: i32,
    pub drunk_level: i32,
}

impl Packet for StatsUpdate {
    const ID: u8 = SyncPacketId::StatsUpdate as u8;
}

impl Encode for StatsUpdate {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_i32(self.money);
        w.write_i32(self.drunk_level);
        w.into_bytes()
    }
}

/// Outgoing spectator sync body (`ID_SPECTATOR_SYNC` = 212): `lrAnalog:u16, udAnalog:u16, keys:u16,
/// position:3xf32` (18 bytes). The real Arizona client spectates — sending this — while it answers the
/// login dialog, and only spawns afterwards; sending on-foot sync before login earns "ОШИБКА 7721".
/// The id byte is prepended by the transport.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpectatorSync {
    pub position: Vector3,
}

impl Packet for SpectatorSync {
    const ID: u8 = SyncPacketId::SpectatorSync as u8;
}

impl Encode for SpectatorSync {
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_u16(0); // lrAnalog
        w.write_u16(0); // udAnalog
        w.write_u16(0); // keys
        w.write_f32(self.position.x);
        w.write_f32(self.position.y);
        w.write_f32(self.position.z);
        w.into_bytes()
    }
}

/// Outgoing on-foot sync body (`ID_PLAYER_SYNC` = 207).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OnFootSync {
    pub keys: u16,
    pub position: Vector3,
    pub quaternion: Quaternion,
    pub health: u8,
    pub armour: u8,
    pub weapon: WeaponId,
    pub special_action: u8,
    /// Reported velocity (moveSpeed@38). A moving `position` with a zero `move_speed` reads as a
    /// teleport to the server's anti-cheat, so movement must set a matching velocity here.
    pub move_speed: Vector3,
    /// Animation index/flags (animIndex@64, animFlags@66). The real client sends a non-zero pair.
    pub animation_id: u16,
    pub animation_flags: u16,
}

/// On-foot sync body length in bytes (544 bits), per the on-foot branch of Net_SendInGameSync
/// (0x455210).
pub const ON_FOOT_SYNC_LEN: usize = 68;

impl Packet for OnFootSync {
    const ID: u8 = SyncPacketId::PlayerSync as u8;
}

impl Encode for OnFootSync {
    /// Encode the 544-bit on-foot sync body. Layout (byte offsets) ported from Net_SendInGameSync's
    /// on-foot branch:
    /// `lrAnalog:u16@0, udAnalog:u16@2, keys:u16@4, position:3xf32@6, quaternion:4xf32@18,
    /// health:u8@34, armour:u8@35, weapon:u8@36, special_action:u8@37, moveSpeed:3xf32@38,
    /// surfOffset:3xf32@50, surfVehicle:u16@62, animIndex:u16@64, animFlags:u16@66`.
    ///
    /// The id byte is prepended by the caller/transport.
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        // Left/right + up/down analog steering: the original sender zeroes these.
        w.write_u16(0);
        w.write_u16(0);
        w.write_u16(self.keys);
        w.write_f32(self.position.x);
        w.write_f32(self.position.y);
        w.write_f32(self.position.z);
        // TODO(verify): quaternion component order — modelled as (x, y, z, w) to match `Quaternion`;
        // the binary copies 16 raw bytes from the rotation state.
        w.write_f32(self.quaternion.x);
        w.write_f32(self.quaternion.y);
        w.write_f32(self.quaternion.z);
        w.write_f32(self.quaternion.w);
        w.write_u8(self.health);
        w.write_u8(self.armour);
        w.write_u8(self.weapon.0);
        w.write_u8(self.special_action);
        // Move speed (x, y, z): the reported velocity — nonzero while the bot is moving so the
        // server's anti-cheat sees a plausible velocity backing the position delta, not a teleport.
        w.write_f32(self.move_speed.x);
        w.write_f32(self.move_speed.y);
        w.write_f32(self.move_speed.z);
        // Surfing offset (x, y, z): unmodelled, zeroed.
        w.write_f32(0.0);
        w.write_f32(0.0);
        w.write_f32(0.0);
        w.write_u16(0); // surfing vehicle id
        w.write_u16(self.animation_id); // animation index (real client: 0x04A5 on foot)
        w.write_u16(self.animation_flags); // animation flags
        w.into_bytes()
    }
}

/// Arizona's custom on-foot position report (`packet 221`, sub-id `53`) — a 28-byte, byte-aligned
/// sync the Arizona client streams alongside stock on-foot sync (207). The server anti-cheat kicks a
/// client that never sends it ("Игнорирование функции(52 / 1)", where 52 is the inbound 221 sub-id).
/// Layout reversed from live capture, all little-endian:
/// `[u8 221][u8 53][u8 0][u16 entity_id][f32 x][f32 y][f32 z][u32 timestamp_ms][4B velocity]
/// [u16 heading][u8 0x80]`. `timestamp_ms` must strictly increase (the server's replay/stall guard);
/// `velocity`/`heading` carry the rest values ([`Self::REST_VELOCITY`]/[`Self::REST_HEADING`]) when
/// the player is stationary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArizonaSync221 {
    pub entity_id: u16,
    pub position: Vector3,
    pub timestamp_ms: u32,
    pub velocity: [u8; 4],
    pub heading: u16,
}

impl ArizonaSync221 {
    /// The packet id (221) and sub-id (53) that open the body.
    pub const PACKET_ID: u8 = 221;
    pub const SUB_ID: u8 = 53;
    /// Rest-state movement values (captured from a stationary real client).
    pub const REST_VELOCITY: [u8; 4] = [0, 0, 0, 0];
    pub const REST_HEADING: u16 = 0xFF7F;

    /// Encode the 28-byte packet. The id byte is included — this is a raw packet, not an RPC body.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::with_capacity(28);
        w.push(Self::PACKET_ID);
        w.push(Self::SUB_ID);
        w.push(0x00);
        w.extend_from_slice(&self.entity_id.to_le_bytes());
        w.extend_from_slice(&self.position.x.to_le_bytes());
        w.extend_from_slice(&self.position.y.to_le_bytes());
        w.extend_from_slice(&self.position.z.to_le_bytes());
        w.extend_from_slice(&self.timestamp_ms.to_le_bytes());
        w.extend_from_slice(&self.velocity);
        w.extend_from_slice(&self.heading.to_le_bytes());
        w.push(0x80);
        w
    }
}

/// A server→client coloured chat/system line (`RPC_ClientMessage` = 93). `text` is raw bytes in the
/// server's encoding (cp1251 on Russian SA-MP servers); decode with [`crate::decode_cp1251`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerMessage {
    /// RGBA colour as sent on the wire (`0xRRGGBBAA`).
    pub color: u32,
    pub text: Vec<u8>,
}

impl Packet for ServerMessage {
    const ID: u8 = RpcId::ClientMessage as u8;
}

impl Decode for ServerMessage {
    /// Decode `RPC_ClientMessage` (93): `[u32 colour LE][u32 len LE][text]`. Verified against Arizona
    /// Bumble Bee live (`ffffffff 01000000 20` ⇒ white, len 1, `" "`).
    ///
    /// ```
    /// use samp_proto::{Decode, ServerMessage};
    /// let msg = ServerMessage::decode(&[0xff,0xff,0xff,0xff, 0x02,0,0,0, b'h', b'i']).unwrap();
    /// assert_eq!(msg.color, 0xffff_ffff);
    /// assert_eq!(msg.text, b"hi");
    /// ```
    fn decode(payload: &[u8]) -> Result<Self> {
        let mut r = BitStreamReader::new(payload);
        let color = r.read_u32()?;
        let len = r.read_u32()? as usize;
        let text = r.read_bytes(len)?;
        Ok(ServerMessage { color, text })
    }
}

impl Encode for ServerMessage {
    /// Re-encode a `RPC_ClientMessage` (inverse of [`ServerMessage::decode`]) so the mock server can
    /// send coloured lines: `[u32 colour LE][u32 len LE][text]`.
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_u32(self.color);
        w.write_u32(self.text.len() as u32);
        w.write_bytes(&self.text);
        w.into_bytes()
    }
}

/// A server→client player chat broadcast (`RPC_Chat` = 101). `text` is raw bytes (see
/// [`crate::decode_cp1251`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub player_id: PlayerId,
    pub text: Vec<u8>,
}

impl Packet for ChatMessage {
    const ID: u8 = RpcId::Chat as u8;
}

impl Decode for ChatMessage {
    /// Decode a server→client `RPC_Chat` (101) broadcast: `[u16 playerId LE][u8 len][text]`.
    ///
    /// ```
    /// use samp_proto::{ChatMessage, Decode};
    /// let msg = ChatMessage::decode(&[0x05, 0x00, 0x02, b'y', b'o']).unwrap();
    /// assert_eq!(msg.player_id.0, 5);
    /// assert_eq!(msg.text, b"yo");
    /// ```
    fn decode(payload: &[u8]) -> Result<Self> {
        let mut r = BitStreamReader::new(payload);
        let player_id = PlayerId(r.read_u16()?);
        let len = r.read_u8()? as usize;
        let text = r.read_bytes(len)?;
        Ok(ChatMessage { player_id, text })
    }
}

impl Encode for ChatMessage {
    /// Re-encode a player chat broadcast (inverse of [`ChatMessage::decode`]) so the mock server can
    /// send it: `[u16 playerId LE][u8 len][text]`. The length is a single byte, so `text` is
    /// truncated to 255 bytes.
    fn encode(&self) -> Vec<u8> {
        let len = self.text.len().min(u8::MAX as usize);
        let mut w = BitStreamWriter::new();
        w.write_u16(self.player_id.0);
        w.write_u8(len as u8);
        w.write_bytes(&self.text[..len]);
        w.into_bytes()
    }
}

/// An outgoing `RPC_Chat` (101) — what the client sends when the local player types in the chat bar:
/// `[u8 len][text]`. `text` is raw bytes in the server's encoding; the length is a single byte, so
/// callers must keep messages ≤ 255 bytes (longer input is truncated).
#[derive(Debug, Clone)]
pub struct ChatOutgoing<'a> {
    pub text: &'a [u8],
}

impl Packet for ChatOutgoing<'_> {
    const ID: u8 = RpcId::Chat as u8;
}

impl Encode for ChatOutgoing<'_> {
    fn encode(&self) -> Vec<u8> {
        let len = self.text.len().min(u8::MAX as usize);
        let mut w = BitStreamWriter::new();
        w.write_u8(len as u8);
        w.write_bytes(&self.text[..len]);
        w.into_bytes()
    }
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

impl Packet for ShowDialog {
    const ID: u8 = RpcId::ShowDialog as u8;
}

impl Decode for ShowDialog {
    /// Decode the structural head of a `ShowDialog` (61): `[u16 dialogId][u8 style][str8 title]
    /// [str8 button1][str8 button2]…`. The trailing body string is left undecoded.
    fn decode(payload: &[u8]) -> Result<Self> {
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
}

impl Encode for ShowDialog {
    /// Re-encode the structural head of a `ShowDialog` (inverse of [`ShowDialog::decode`]) so the
    /// mock server can prompt a dialog. The undecoded trailing body is omitted (decode ignores it).
    fn encode(&self) -> Vec<u8> {
        let mut w = BitStreamWriter::new();
        w.write_u16(self.dialog_id);
        w.write_u8(self.style);
        write_str8_bytes(&mut w, &self.title);
        write_str8_bytes(&mut w, &self.button1);
        write_str8_bytes(&mut w, &self.button2);
        w.into_bytes()
    }
}

impl ShowDialog {
    /// Decode the dialog BODY (the info text / list rows) that [`Self::decode`] skips — it follows the
    /// head fields and is SA-MP Huffman-encoded (StringCompressor), same as 3D-text-label text. For
    /// list/tablist dialogs this is the selectable options, `\n`-separated. Returns raw cp1251 bytes
    /// (empty if the head is malformed or there is no body). Kept off the hot `decode` path so the
    /// structural response path stays allocation-light.
    pub fn body(payload: &[u8]) -> Vec<u8> {
        // The head is byte-aligned: `[u16 dialogId][u8 style][u8 tLen][title][u8 b1Len][b1][u8 b2Len]
        // [b2]`. Walk it by byte offset to find where the body starts, then Huffman-decode the rest
        // (the same StringCompressor `decode_string` that `readEncoded` uses).
        let mut off = 3usize; // [u16 dialog_id] + [u8 style]
        for _ in 0..3 {
            let len = match payload.get(off) {
                Some(&l) => l as usize,
                None => return Vec::new(),
            };
            off += 1 + len;
        }
        crate::encoded::decode_string(payload.get(off..).unwrap_or(&[]), 4096)
    }
}

fn read_str8_bytes(r: &mut BitStreamReader) -> Result<Vec<u8>> {
    let len = r.read_u8()? as usize;
    r.read_bytes(len)
}

fn write_str8_bytes(w: &mut BitStreamWriter, bytes: &[u8]) {
    let len = bytes.len().min(u8::MAX as usize);
    w.write_u8(len as u8);
    w.write_bytes(&bytes[..len]);
}

/// An outgoing `RPC_DialogResponse` (62): `[u16 dialogId][u8 button][u16 listItem][u8 len][text]`.
/// `button` is `1` for the positive/left button (login/OK), `0` for the right/cancel; `list_item`
/// is the selected row (`0xFFFF` if none); `input` is the text-box content (e.g. the account
/// password), truncated to 255 bytes.
#[derive(Debug, Clone)]
pub struct DialogResponse<'a> {
    pub dialog_id: u16,
    pub button: u8,
    pub list_item: u16,
    pub input: &'a [u8],
}

impl Packet for DialogResponse<'_> {
    const ID: u8 = RpcId::DialogResponse as u8;
}

impl Encode for DialogResponse<'_> {
    fn encode(&self) -> Vec<u8> {
        let len = self.input.len().min(u8::MAX as usize);
        let mut w = BitStreamWriter::new();
        w.write_u16(self.dialog_id);
        w.write_u8(self.button);
        w.write_u16(self.list_item);
        w.write_u8(len as u8);
        w.write_bytes(&self.input[..len]);
        w.into_bytes()
    }
}

/// Read the assigned player id + server cookie from a `CONNECTION_REQUEST_ACCEPTED` body.
///
/// Verified against samp.dll sub_1000AA20: the body (after the RakNet id byte) is `[u32 external IP]
/// [u16 port][u16 systemIndex][u32 cookie]`. The systemIndex is the assigned local player id; the
/// cookie XORed with [`crate::CHALLENGE_XOR`] (0xFD9) becomes the `ClientJoin` challenge response.
pub fn parse_connect(body: &[u8]) -> Result<(PlayerId, ServerCookie)> {
    let mut reader = BitStreamReader::new(body);
    let _ip = reader.read_u32()?;
    let _port = reader.read_u16()?;
    let player_id = PlayerId(reader.read_u16()?);
    let cookie = ServerCookie(reader.read_u32()?);
    Ok((player_id, cookie))
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
