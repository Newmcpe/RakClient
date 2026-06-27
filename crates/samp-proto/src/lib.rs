//! SA-MP 0.3.7 wire protocol — pure codecs, no I/O.
//!
//! This crate is the **public contract** other crates compile against. The bit-packing and the
//! RPC/sync layouts are ports of the original 0.3.7 client (verified against the binary's
//! `BitStream_WriteBits`/`BitStream_ReadBits`, the `RPC_ClientJoin` handshake, `RPC_InitGame`,
//! the class/spawn request paths, and the on-foot branch of the in-game sync sender).
//!
//! Provenance highlights (verified in the binary):
//! - RakNet bit order: bits are packed MSB-first within a byte; multi-byte integers are stored in
//!   little-endian byte order, then bit-packed.
//! - `ClientJoin`: `version = 4057`, `challenge_response = server_cookie ^ 0xFD9`.
//! - On-foot sync body is exactly 544 bits / 68 bytes.
#![forbid(unsafe_code)]

mod bitstream;
mod codec;
mod encoded;
pub mod events;
mod fields;
mod ids;
mod rwbitstream;
mod sync;

pub use bitstream::{BitStreamReader, BitStreamWriter};
pub use encoded::{decode_string, encode_string};
pub use fields::Vector2;
pub use rwbitstream::RwBitStream;
pub use sync::{
    AimSyncData, BulletSyncData, PassengerSyncData, PlayerSyncData, TrailerSyncData,
    UnoccupiedSyncData, VehicleSyncData,
};

pub use codec::{
    build_az_game_path, decode_client_message, decode_init_game, decode_player_chat,
    decode_request_class_response, decode_request_spawn_response, decode_show_dialog,
    encode_az_cef_message, encode_az_client_id, encode_az_focus, encode_az_game_path,
    encode_az_resolution, encode_chat, encode_client_join, encode_dialog_response,
    encode_on_foot_sync, encode_request_class, encode_request_spawn, encode_spawn, generate_gpci,
    generate_gpci_seeded, generate_user_id, generate_user_id_seeded, ChatMessage, ClientJoin,
    InitGame, OnFootSync, RequestClassResponse, RequestSpawnResponse, ServerMessage, ShowDialog,
    AZ_PACKET_ID, ON_FOOT_SYNC_LEN,
};
pub use ids::{RpcId, SyncPacketId};
#[doc(hidden)]
pub use inventory;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ProtoError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("bit stream exhausted: needed {needed} bits, had {available}")]
    Exhausted { needed: usize, available: usize },
    #[error("invalid UTF-8 in protocol string")]
    InvalidString,
    #[error("unknown RPC id {0}")]
    UnknownRpc(u8),
    #[error("unknown packet id {0}")]
    UnknownPacket(u8),
    #[error("malformed packet: {0}")]
    Malformed(&'static str),
}

// Domain newtypes — carry meaning so the compiler rejects mixing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct PlayerId(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ClassId(pub i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Skin(pub u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WeaponId(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ServerCookie(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Quaternion {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

/// Direction of a packet/RPC relative to the local client. Incoming and outgoing share no id space,
/// so dispatch keys on this alongside the id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Server → client.
    Incoming,
    /// Client → server.
    Outgoing,
}

/// A message a script asked the host to send (via `sampSendPacket`/`sampSendRpc`). The driver drains
/// these and puts them on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundMsg {
    /// A raw application packet (`data[0]` = id). `reliability` is the RakNet wire value
    /// (`0..=4` = Unreliable/UnreliableSequenced/Reliable/ReliableOrdered/ReliableSequenced) so
    /// this crate stays raknet-free; the driver maps it to `raknet::Reliability`. `sendPacket()`
    /// defaults to reliable-ordered (`3`) on channel `0` — the Arizona `220` path.
    Packet {
        data: Vec<u8>,
        reliability: u8,
        channel: u8,
    },
    /// An RPC by id with a body bitstream.
    Rpc { id: u8, payload: Vec<u8> },
}

/// RakNet wire value for `ReliableOrdered` — the default for `sendPacket()` and the Arizona path.
pub const RELIABILITY_RELIABLE_ORDERED: u8 = 3;

/// Shared queue of script-initiated sends: the script VM pushes, the driver drains. `!Send` —
/// single thread.
pub type Outbox = std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<OutboundMsg>>>;

/// A handler's decision about a packet/RPC body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Forward the body unchanged.
    Pass,
    /// Consume the packet — it is neither processed further nor forwarded.
    Drop,
    /// Replace the body with these bytes before processing/forwarding.
    Rewrite(Vec<u8>),
}

/// 0.3.7 protocol version sent as the first `ClientJoin` field.
pub const SAMP_VERSION_0_3_7: u32 = 4057;
/// `challenge_response = server_cookie ^ CHALLENGE_XOR` (verified in the join sender).
pub const CHALLENGE_XOR: u32 = 0xFD9;

/// Unicode scalar values for Windows-1251 (cp1251) bytes `0x80..=0xFF`. SA-MP Russian servers
/// (Arizona) send chat/system text in cp1251, so chat bytes must be transcoded for display.
/// `0x98` is unassigned in cp1251 and maps to the replacement character. `0xC0..=0xFF` is the
/// contiguous `А..я` block and is computed rather than tabled.
#[rustfmt::skip]
const CP1251_HIGH: [char; 64] = [
    'Ђ','Ѓ','‚','ѓ','„','…','†','‡','€','‰','Љ','‹','Њ','Ќ','Ћ','Џ',
    'ђ','‘','’','“','”','•','–','—','\u{FFFD}','™','љ','›','њ','ќ','ћ','џ',
    '\u{00A0}','Ў','ў','Ј','¤','Ґ','¦','§','Ё','©','Є','«','¬','\u{00AD}','®','Ї',
    '°','±','І','і','ґ','µ','¶','·','ё','№','є','»','ј','Ѕ','ѕ','ї',
];

/// Decode a Windows-1251 (cp1251) byte string to a Rust `String`. ASCII (`< 0x80`) passes through;
/// `0x80..=0xBF` use [`CP1251_HIGH`]; `0xC0..=0xFF` map linearly to `U+0410..=U+044F`. Lossless for
/// all defined cp1251 bytes — used to render SA-MP chat ([`ServerMessage`]/[`ChatMessage`]) text.
///
/// ```
/// use samp_proto::decode_cp1251;
/// assert_eq!(decode_cp1251(b"hi"), "hi");
/// assert_eq!(decode_cp1251(&[0xCF, 0xF0, 0xE8]), "При");
/// ```
pub fn decode_cp1251(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x00..=0x7F => b as char,
            0x80..=0xBF => CP1251_HIGH[(b - 0x80) as usize],
            0xC0..=0xFF => char::from_u32(0x0410 + (b as u32 - 0xC0)).unwrap_or('\u{FFFD}'),
        })
        .collect()
}
