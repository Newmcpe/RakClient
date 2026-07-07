//! SA-MP 0.3.7 wire protocol — pure codecs, no I/O. The public contract other crates compile
//! against; reversed provenance highlights: see docs/memory/samp-proto/lib.md#module-provenance
#![forbid(unsafe_code)]

mod bitstream;
mod codec;
mod encoded;
pub mod events;
mod fields;
mod ids;
mod rwbitstream;
mod sync;
mod weapon;

pub use bitstream::{BitStreamReader, BitStreamWriter};
pub use encoded::{decode_string, encode_string};
pub use fields::Vector2;
pub use rwbitstream::RwBitStream;
pub use sync::{
    AimSyncData, BulletSyncData, PassengerSyncData, PlayerSyncData, TrailerSyncData,
    UnoccupiedSyncData, VehicleSyncData,
};

pub use codec::{
    generate_gpci, generate_gpci_seeded, parse_connect, ArizonaSync221, ChatMessage, ChatOutgoing,
    ClientJoin, Decode, DialogResponse, Encode, InitGame, OnFootSync, Packet, RequestClass,
    RequestClassResponse, RequestSpawn, RequestSpawnResponse, ServerMessage, ShowDialog, Spawn,
    SpectatorSync, StatsUpdate, ON_FOOT_SYNC_LEN,
};
pub use ids::{RpcId, SyncPacketId};
#[doc(hidden)]
pub use inventory;
pub use weapon::{encode_weapons_update, weapon_slot, weapon_state, WeaponSlot, WEAPON_SLOTS};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ProtoError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("bit stream exhausted: needed {needed} bits, had {available}")]
    Exhausted { needed: usize, available: usize },
    #[error("invalid UTF-8 in protocol string")]
    InvalidString,
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
    /// A raw application packet (`data[0]` = id); `reliability`/`channel` map to `raknet::Reliability`.
    /// See docs/memory/samp-proto/lib.md#outboundmsg-packet
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

/// Unicode scalar values for cp1251 bytes `0x80..=0xFF`; see docs/memory/samp-proto/lib.md#cp1251-high
#[rustfmt::skip]
const CP1251_HIGH: [char; 64] = [
    'Ђ','Ѓ','‚','ѓ','„','…','†','‡','€','‰','Љ','‹','Њ','Ќ','Ћ','Џ',
    'ђ','‘','’','“','”','•','–','—','\u{FFFD}','™','љ','›','њ','ќ','ћ','џ',
    '\u{00A0}','Ў','ў','Ј','¤','Ґ','¦','§','Ё','©','Є','«','¬','\u{00AD}','®','Ї',
    '°','±','І','і','ґ','µ','¶','·','ё','№','є','»','ј','Ѕ','ѕ','ї',
];

/// Decode a Windows-1251 (cp1251) byte string to a `String` (lossless for defined bytes); renders SA-MP chat.
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

/// Encode a UTF-8 string to Windows-1251 (cp1251) bytes — inverse of [`decode_cp1251`]; unmapped chars become `?`.
///
/// ```
/// use samp_proto::encode_cp1251;
/// assert_eq!(encode_cp1251("hi"), b"hi");
/// assert_eq!(encode_cp1251("При"), vec![0xCF, 0xF0, 0xE8]);
/// ```
pub fn encode_cp1251(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c {
            '\u{0}'..='\u{7F}' => c as u8,
            'А'..='я' => (0xC0 + (c as u32 - 0x0410)) as u8,
            _ => CP1251_HIGH
                .iter()
                .position(|&h| h == c)
                .map(|i| 0x80 + i as u8)
                .unwrap_or(b'?'),
        })
        .collect()
}
