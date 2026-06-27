//! Server-side SA-MP/RakNet 3.x framing used by [`crate::MockSampServer`].
//!
//! The mock talks raw UDP to the client's [`raknet`] transport, so every byte on the wire goes
//! through the one true implementation the client uses:
//!
//! * the per-datagram cipher and the reliability-datagram framing come from [`raknet::cipher`] and
//!   [`raknet::wire`] (the mock owns a [`raknet::wire::ReliabilityLayer`] in [`crate::server`]);
//! * the offline handshake id bytes and the [`raknet::wire::ID_RPC`] envelope are the shared
//!   constants from [`raknet::wire`];
//! * this module only contributes the scripted *payloads* of the connect → spawn sequence and the
//!   recognition of inbound on-foot sync packets (ids `200..=212`).
//!
//! The connect → spawn RPC payloads are reproduced byte-for-byte from the original client's receive
//! handlers (`RPC_InitGame`, `RPC_RequestClassResponse`, `RPC_RequestSpawnResponse`), and the
//! `CONNECTION_REQUEST_ACCEPTED` body matches the client's `parse_connect`, so a real
//! [`samp_proto`] decoder reads back exactly the values the mock scripted.

use raknet::wire::{ID_CONNECTION_REQUEST, ID_RPC};
use samp_proto::{BitStreamWriter, ClassId, PlayerId, RpcId, ServerCookie};

/// Number of header bits the `CONNECTION_REQUEST_ACCEPTED` body carries before the assigned player
/// id: a RakNet system address = external IP (`u32`) + port (`u16`) = 48 bits. The client's
/// `parse_connect` reads exactly `[u32 ip][u16 port][u16 player id][u32 cookie]`.
pub const CRA_HEADER_SKIP_BITS: usize = 48;

/// Bit offset of the `u16` local player id inside an `InitGame` payload. Verified in `RPC_InitGame`.
pub const INITGAME_PLAYER_ID_BIT_OFFSET: usize = 104;

#[inline]
fn msg_id(value: raknet::MessageId) -> u8 {
    value as u8
}

/// Decrypt a received datagram with the shared port-keyed cipher.
pub fn decrypt(datagram: &[u8], port: u16) -> raknet::Result<Vec<u8>> {
    raknet::cipher::decrypt(datagram, port)
}

/// A parsed inbound message — already decrypted *and* lifted out of the reliability layer. The
/// unframed offline open-connection probe is handled before reliability in [`crate::server`] and so
/// never reaches here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    ConnectionRequest,
    Rpc { id: u8, payload: Vec<u8> },
    Sync { id: u8 },
    Disconnect,
    Other(u8),
}

/// Parse a single delivered message (post-cipher, post-reliability) into a high-level [`Inbound`].
pub fn parse(message: &[u8]) -> Option<Inbound> {
    let (&head, _) = message.split_first()?;
    let parsed = match head {
        ID_CONNECTION_REQUEST => Inbound::ConnectionRequest,
        ID_RPC => {
            let (id, payload) = raknet::wire::parse_rpc(message)?;
            Inbound::Rpc { id, payload }
        }
        200..=212 => Inbound::Sync { id: head },
        id if id == msg_id(raknet::MessageId::DisconnectionNotification) => Inbound::Disconnect,
        other => Inbound::Other(other),
    };
    Some(parsed)
}

/// `ID_OPEN_CONNECTION_REPLY` (2 bytes) — accepts the offline open-connection probe (sent in the
/// clear).
pub fn open_connection_reply() -> Vec<u8> {
    vec![raknet::wire::ID_OPEN_CONNECTION_REPLY, 0]
}

/// `CONNECTION_REQUEST_ACCEPTED` (34) body after the id byte: `[u32 ip][u16 port][u16 player id]
/// [u32 cookie]`, matching the client's `parse_connect`. IP/port are unused by the client (zeroed);
/// the player id is the assigned system index and the cookie drives the join challenge
/// (`cookie ^ 0xFD9`).
pub fn connection_request_accepted(player_id: PlayerId, cookie: ServerCookie) -> Vec<u8> {
    let mut out = vec![msg_id(raknet::MessageId::ConnectionRequestAccepted)];
    let mut bs = BitStreamWriter::new();
    bs.write_u32(0); // external system IP (ignored by the client)
    bs.write_u16(0); // external system port (ignored by the client)
    bs.write_u16(player_id.0); // assigned player id / system index
    bs.write_u32(cookie.0); // server cookie
    out.extend_from_slice(&bs.into_bytes());
    out
}

/// A RakNet system rejection used for the `reject_connection` fault.
pub fn rejection() -> Vec<u8> {
    vec![msg_id(raknet::MessageId::NoFreeIncomingConnections)]
}

/// Frame a SA-MP RPC into a message body via the shared [`raknet::wire::build_rpc`]:
/// `[ID_RPC][rpc_id][bit_len: u32 le][payload]`.
pub fn rpc(rpc_id: RpcId, payload: &[u8]) -> Vec<u8> {
    raknet::wire::build_rpc(rpc_id as u8, payload)
}

/// `RPC_InitGame` (139) payload. Field order verified against the client's `RPC_InitGame` reader;
/// only the player id (`u16` at bit 104) and the host name are meaningful, the rest is server
/// settings the mock leaves zeroed.
pub fn init_game_payload(player_id: PlayerId, host_name: &str) -> Vec<u8> {
    let mut bs = BitStreamWriter::new();
    write_bits(&mut bs, 4); // four boolean settings
    bs.write_u32(0); // (A)
    write_bits(&mut bs, 1);
    bs.write_u32(0); // (B)
    write_bits(&mut bs, 3);
    bs.write_u32(0); // (C)
    bs.write_u16(player_id.0); // (D) assigned player id @ bit 104
    write_bits(&mut bs, 1);
    bs.write_u32(0); // (E)
    bs.write_u8(0); // (F) world time
    bs.write_u8(0); // (G) weather
    bs.write_u32(0); // (H) gravity
    write_bits(&mut bs, 1);
    bs.write_u32(0); // (I)
    write_bits(&mut bs, 1);
    bs.write_u32(0); // (J) 128-bit reserved block
    bs.write_u32(0);
    bs.write_u32(0);
    bs.write_u32(0);
    bs.write_u8(0); // (K)
    bs.write_u8(0); // (L)
    bs.write_u8(0); // (M)
    bs.write_u8(0); // (N)
    bs.write_str8(host_name); // (O) host name (u8 length prefix)
    bs.write_bytes(&[0u8; 212]); // (P) trailing settings blob
    bs.into_bytes()
}

/// `RPC_RequestClassResponse` (128) payload: `[u8 allowed][46-byte spawn info]`.
pub fn request_class_response_payload(_class: ClassId) -> Vec<u8> {
    let mut bs = BitStreamWriter::new();
    bs.write_u8(1); // allowed
    bs.write_bytes(&[0u8; 46]); // spawn info (team/skin/pos/angle/weapons), zeroed
    bs.into_bytes()
}

/// `RPC_RequestSpawnResponse` (129) payload: a single allow byte (`2` = spawn now).
pub fn request_spawn_response_payload() -> Vec<u8> {
    let mut bs = BitStreamWriter::new();
    bs.write_u8(2);
    bs.into_bytes()
}

fn write_bits(bs: &mut BitStreamWriter, count: usize) {
    for _ in 0..count {
        bs.write_bit(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_frame_round_trips() {
        let payload = [0xAA, 0xBB, 0xCC];
        let frame = rpc(RpcId::RequestSpawn, &payload);
        match parse(&frame) {
            Some(Inbound::Rpc { id, payload: got }) => {
                assert_eq!(id, RpcId::RequestSpawn as u8);
                assert_eq!(got, payload);
            }
            other => panic!("expected Rpc, got {other:?}"),
        }
    }

    #[test]
    fn sync_id_is_recognised() {
        let datagram = [samp_proto::SyncPacketId::PlayerSync as u8, 0, 1, 2];
        assert_eq!(
            parse(&datagram),
            Some(Inbound::Sync {
                id: samp_proto::SyncPacketId::PlayerSync as u8
            })
        );
    }

    #[test]
    fn connection_request_accepted_matches_parse_connect() {
        let body = connection_request_accepted(PlayerId(7), ServerCookie(0x5AC0_FFEE));
        // Skip the leading id byte; the rest is what the client's `parse_connect` reads.
        let mut r = samp_proto::BitStreamReader::new(&body[1..]);
        assert_eq!(r.read_u32().unwrap(), 0); // ip
        assert_eq!(r.read_u16().unwrap(), 0); // port
        assert_eq!(r.read_u16().unwrap(), 7); // player id
        assert_eq!(r.read_u32().unwrap(), 0x5AC0_FFEE); // cookie
    }
}
