//! Public wire primitives shared by [`RakPeer`](crate::RakPeer) and any peer that must speak the
//! same bytes (e.g. the loopback mock server in `test-support`).
//!
//! This is the single source of truth for the parts of the protocol both ends of a connection have
//! to agree on byte-for-byte: the RakNet message id bytes, the reliability-datagram framing
//! ([`ReliabilityLayer`], re-exported from [`crate::reliability`]), and the SA-MP RPC envelope.
//!
//! Id values and the framing are the SA-MP / RakNet 3.x ("legacy network", as spoken by open.mp)
//! layout reversed from RakSAMP Lite.

pub use crate::reliability::ReliabilityLayer;

use samp_proto::{BitStreamReader, BitStreamWriter};

/// `ID_OPEN_CONNECTION_REQUEST` — offline probe (a fixed 3-byte packet `[24][0][pad]`).
pub const ID_OPEN_CONNECTION_REQUEST: u8 = 24;
/// `ID_OPEN_CONNECTION_REPLY` — the server's 2-byte acceptance of the probe.
pub const ID_OPEN_CONNECTION_REPLY: u8 = 25;
/// `ID_OPEN_CONNECTION_COOKIE` — server's cookie challenge (open.mp anti-flood) carrying a 4-byte
/// cookie the client must echo in a follow-up open-connection request.
pub const ID_OPEN_CONNECTION_COOKIE: u8 = 26;
/// `ID_CONNECTION_REQUEST` — first reliability-framed message the client sends (id + password).
pub const ID_CONNECTION_REQUEST: u8 = 11;
/// `ID_PONG` — reply to a `ID_PING`; surfaced so the trace can label stray pings.
pub const ID_PONG: u8 = 39;

/// `ID_INTERNAL_PING` — the server's periodic *connected* ping (distinct from the offline
/// `ID_PING`/`ID_PONG` query). The client must answer with [`ID_CONNECTED_PONG`] so the server can
/// measure our ping; an unanswered one leaves the server's ping stat at `0xFFFF` and reads as an
/// ignored client function to Arizona's anti-cheat. Body: `[6][u32 send time]`.
pub const ID_INTERNAL_PING: u8 = 6;
/// `ID_CONNECTED_PONG` — the client's reply to [`ID_INTERNAL_PING`]:
/// `[9][u32 echoed send time][u32 local time]`.
pub const ID_CONNECTED_PONG: u8 = 9;

/// SA-MP `ID_RPC` — marks a reliability message whose body is an RPC envelope.
pub const ID_RPC: u8 = 20;
/// `ID_TIMESTAMP` — optional prefix (`id + u32 time`) some RPCs carry before [`ID_RPC`].
pub const ID_TIMESTAMP: u8 = 40;

/// Frame a SA-MP RPC body into a reliability message payload:
/// `[ID_RPC][unique_id][WriteCompressed(bit_length: u32)][payload bits]`.
///
/// The payload bits follow the compressed length without re-aligning, exactly as RakNet's
/// `RakPeer::RPC` writes them.
pub fn build_rpc(unique_id: u8, payload: &[u8]) -> Vec<u8> {
    let mut w = BitStreamWriter::new();
    w.write_u8(ID_RPC);
    w.write_u8(unique_id);
    let bit_length = (payload.len() as u32).saturating_mul(8);
    w.write_compressed_u32(bit_length);
    w.write_bytes(payload);
    w.into_bytes()
}

/// Build the [`ID_CONNECTED_PONG`] reply to a server [`ID_INTERNAL_PING`]. `ping` is the received
/// message (`[6][u32 send time]`), `local_ms` our millisecond clock; the server's send time is
/// echoed byte-for-byte so its round-trip calculation stays exact. Returns `None` when `ping` is too
/// short. Layout verified against `samp.dll`'s `RakPeer::RunUpdateCycle`:
/// `[9][u32 echoed send time][u32 local time]`.
pub fn build_connected_pong(ping: &[u8], local_ms: u32) -> Option<Vec<u8>> {
    let echoed = ping.get(1..5)?;
    let mut pong = Vec::with_capacity(9);
    pong.push(ID_CONNECTED_PONG);
    pong.extend_from_slice(echoed);
    pong.extend_from_slice(&local_ms.to_le_bytes());
    Some(pong)
}

/// Parse a reliability message that begins with [`ID_RPC`] (optionally preceded by an
/// [`ID_TIMESTAMP`] block) into `(unique_id, payload)`.
pub fn parse_rpc(message: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut r = BitStreamReader::new(message);
    let mut id = r.read_u8().ok()?;
    if id == ID_TIMESTAMP {
        let _timestamp = r.read_u32().ok()?;
        id = r.read_u8().ok()?;
    }
    if id != ID_RPC {
        return None;
    }
    let unique_id = r.read_u8().ok()?;
    let bit_length = r.read_compressed_u32().ok()? as usize;
    // RPC bodies are written with `WriteBits(data, bitLength, false)` — exactly `bit_length` bits,
    // not padded to a byte — so read that many bits rather than `ceil(bit_length / 8)` bytes.
    let payload = r.read_bits_bytes(bit_length).ok()?;
    Some((unique_id, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_frame_round_trips() {
        let frame = build_rpc(25, &[1, 2, 3, 4, 5]);
        let (id, payload) = parse_rpc(&frame).expect("parses");
        assert_eq!(id, 25);
        assert_eq!(payload, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn rpc_frame_handles_empty_payload() {
        let frame = build_rpc(0x42, &[]);
        let (id, payload) = parse_rpc(&frame).expect("parses");
        assert_eq!(id, 0x42);
        assert!(payload.is_empty());
    }

    #[test]
    fn rpc_tolerates_leading_timestamp() {
        let mut w = BitStreamWriter::new();
        w.write_u8(ID_TIMESTAMP);
        w.write_u32(0xDEAD_BEEF);
        w.write_u8(ID_RPC);
        w.write_u8(139);
        w.write_compressed_u32(24);
        w.write_bytes(&[0xAA, 0xBB, 0xCC]);
        let frame = w.into_bytes();

        let (id, payload) = parse_rpc(&frame).expect("parses");
        assert_eq!(id, 139);
        assert_eq!(payload, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn parse_rpc_rejects_non_rpc() {
        assert!(parse_rpc(&[34, 0, 0]).is_none());
        assert!(parse_rpc(&[]).is_none());
    }

    #[test]
    fn connected_pong_echoes_send_time_and_appends_local() {
        // Ping `[6][DE AD BE EF]`; the pong echoes those four bytes verbatim then our LE clock.
        let ping = [ID_INTERNAL_PING, 0xDE, 0xAD, 0xBE, 0xEF];
        let pong = build_connected_pong(&ping, 0x1234_5678).expect("builds");
        assert_eq!(
            pong,
            vec![
                ID_CONNECTED_PONG,
                0xDE,
                0xAD,
                0xBE,
                0xEF,
                0x78,
                0x56,
                0x34,
                0x12
            ]
        );
    }

    #[test]
    fn connected_pong_rejects_short_ping() {
        assert!(build_connected_pong(&[ID_INTERNAL_PING, 0x01, 0x02], 0).is_none());
        assert!(build_connected_pong(&[], 0).is_none());
    }
}
