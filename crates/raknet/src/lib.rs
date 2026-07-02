//! SA-MP "RakNet 3.x" UDP transport.
//!
//! This is the hand-written transport SA-MP needs (no existing Rust crate is wire-compatible — they
//! target Minecraft Bedrock's modern RakNet). Responsibilities: the per-datagram **byte cipher**
//! (port-keyed, 256-byte substitution table), the **reliable/ordered** reliability layer (datagram
//! sequencing, ACK/NAK, ordering channels, split/reassembly), the connection **handshake**, and an
//! async [`RakPeer`] actor over a Tokio `UdpSocket`.
//!
//! The cipher (`cipher::encrypt`/`decrypt`) and the substitution/mask tables ([`tables`]) are
//! reverse-engineered **byte-exact** from `Net_EncryptSend` (`sub_419060`) in RakSAMP Lite.exe.
//! The reliability layer follows RakNet 3.x semantics; see [`reliability`] for the wire-format
//! `TODO(verify)` notes where the SA-MP bit-packing differs from this byte-aligned encoding.
#![forbid(unsafe_code)]

use std::net::SocketAddr;

use thiserror::Error;
use tokio::sync::mpsc;

mod auth_table;
mod reliability;
pub mod socks5;
mod tables;
mod transport;
pub mod wire;

pub use socks5::ProxyConfig;

pub type Result<T> = std::result::Result<T, RaknetError>;

#[derive(Debug, Error)]
pub enum RaknetError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("connection timed out")]
    Timeout,
    #[error("connection rejected: {0:?}")]
    Rejected(DisconnectReason),
    #[error("not connected")]
    NotConnected,
    #[error("malformed datagram")]
    Malformed,
    #[error("socks5 proxy error: {0}")]
    Proxy(String),
}

impl From<samp_proto::ProtoError> for RaknetError {
    fn from(_: samp_proto::ProtoError) -> Self {
        RaknetError::Malformed
    }
}

/// RakNet `DefaultMessageIDTypes` values used by this build (from the receive switch in
/// `Net_Receive`/`sub_403BD0`: case 34 = `ConnectionRequestAccepted`, 40 = `Timestamp`,
/// 32/33 = `DisconnectionNotification`/`ConnectionLost`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageId {
    ConnectionAttemptFailed = 29,
    NoFreeIncomingConnections = 31,
    DisconnectionNotification = 32,
    ConnectionLost = 33,
    ConnectionRequestAccepted = 34,
    ConnectionBanned = 36,
    InvalidPassword = 37,
    Timestamp = 40,
}

impl TryFrom<u8> for MessageId {
    type Error = RaknetError;
    fn try_from(value: u8) -> Result<Self> {
        Ok(match value {
            29 => MessageId::ConnectionAttemptFailed,
            31 => MessageId::NoFreeIncomingConnections,
            32 => MessageId::DisconnectionNotification,
            33 => MessageId::ConnectionLost,
            34 => MessageId::ConnectionRequestAccepted,
            36 => MessageId::ConnectionBanned,
            37 => MessageId::InvalidPassword,
            40 => MessageId::Timestamp,
            _ => return Err(RaknetError::Malformed),
        })
    }
}

/// RakNet `PacketReliability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reliability {
    Unreliable = 0,
    UnreliableSequenced = 1,
    Reliable = 2,
    ReliableOrdered = 3,
    ReliableSequenced = 4,
}

impl Reliability {
    /// Map a RakNet wire reliability ordinal (`0..=4`) back to a [`Reliability`]. Unknown values
    /// fall back to reliable-ordered, the safest choice for script-supplied traffic.
    pub fn from_wire(value: u8) -> Self {
        match value {
            0 => Self::Unreliable,
            1 => Self::UnreliableSequenced,
            2 => Self::Reliable,
            4 => Self::ReliableSequenced,
            _ => Self::ReliableOrdered,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    AttemptFailed,
    ServerFull,
    Banned,
    InvalidPassword,
    ClosedByServer,
    ConnectionLost,
    Rejected,
    Timeout,
    Local,
}

impl std::fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::AttemptFailed => "connection attempt failed",
            Self::ServerFull => "server full",
            Self::Banned => "banned",
            Self::InvalidPassword => "invalid password",
            Self::ClosedByServer => "closed by server",
            Self::ConnectionLost => "connection lost",
            Self::Rejected => "connection rejected",
            Self::Timeout => "connection timed out",
            Self::Local => "disconnected locally",
        };
        f.write_str(text)
    }
}

/// The port-keyed SA-MP datagram cipher (`out[0] = checksum`, `out[1+i] = TABLE[data[i]]`, with odd
/// `i` XORed by `port ^ 0xCC`). Tables are extracted byte-exact from the binary.
pub mod cipher {
    use super::{RaknetError, Result};
    use crate::tables::{INVERSE, MASK_BYTE, SUBSTITUTION};

    /// Encrypt `data` for transmission to a peer on `port`.
    ///
    /// Byte-exact port of `Net_EncryptSend` (`sub_419060`): the output is `data.len() + 1` bytes,
    /// where `out[0]` is the checksum `XOR(data[b] & 0xAA)` over all bytes, `out[1+i]` is
    /// `SUBSTITUTION[data[i]]`, and for odd `i` it is additionally XORed with `(port ^ 0xCC)` (low
    /// byte only).
    pub fn encrypt(data: &[u8], port: u16) -> Vec<u8> {
        let key = (port as u8) ^ 0xCC;
        let mut checksum = 0u8;
        for &b in data {
            checksum ^= b & MASK_BYTE;
        }
        let mut out = Vec::with_capacity(data.len() + 1);
        out.push(checksum);
        for (i, &b) in data.iter().enumerate() {
            let mut v = SUBSTITUTION[b as usize];
            if i & 1 == 1 {
                v ^= key;
            }
            out.push(v);
        }
        out
    }

    /// Invert [`encrypt`]. Returns [`RaknetError::Malformed`] if the buffer is empty or the embedded
    /// checksum does not match the recovered plaintext.
    pub fn decrypt(data: &[u8], port: u16) -> Result<Vec<u8>> {
        let (&checksum, body) = data.split_first().ok_or(RaknetError::Malformed)?;
        let key = (port as u8) ^ 0xCC;
        let mut plain = Vec::with_capacity(body.len());
        for (i, &b) in body.iter().enumerate() {
            let c = if i & 1 == 1 { b ^ key } else { b };
            plain.push(INVERSE[c as usize]);
        }
        let mut check = 0u8;
        for &b in &plain {
            check ^= b & MASK_BYTE;
        }
        if check != checksum {
            return Err(RaknetError::Malformed);
        }
        Ok(plain)
    }
}

/// Events surfaced from the transport to the SA-MP layer.
#[derive(Debug, Clone)]
pub enum RakEvent {
    /// Handshake complete (`CONNECTION_REQUEST_ACCEPTED`). `body` is the packet payload after the id
    /// byte, from which the SA-MP layer reads its assigned player id + server cookie.
    Connected {
        body: Vec<u8>,
    },
    /// A received RPC: `id` is the SA-MP RPC id, `payload` the decompressed argument bitstream.
    Rpc {
        id: u8,
        payload: Vec<u8>,
    },
    /// A non-RPC application packet (e.g. sync ids 200..=212). `data[0]` is the id.
    Packet {
        data: Vec<u8>,
    },
    Disconnected(DisconnectReason),
}

#[derive(Debug, Clone, Default)]
pub struct RakConfig {
    pub password: Option<String>,
    /// Optional RakNet "static data" sent with the connection request.
    pub static_data: Vec<u8>,
    /// Optional SOCKS5 proxy to tunnel the UDP game traffic through (fresh source IP).
    pub proxy: Option<ProxyConfig>,
}

/// Cheap-to-clone handle for talking to a running [`RakPeer`].
#[derive(Clone)]
pub struct RakHandle {
    tx: mpsc::Sender<Command>,
}

impl RakHandle {
    /// Send a raw application packet (`data[0]` = id) with the given reliability/channel.
    pub async fn send(&self, data: Vec<u8>, reliability: Reliability, channel: u8) -> Result<()> {
        self.tx
            .send(Command::Send {
                data,
                reliability,
                channel,
            })
            .await
            .map_err(|_| RaknetError::NotConnected)
    }
    /// Send a SA-MP RPC (reliable-ordered).
    pub async fn rpc(&self, rpc_id: u8, payload: Vec<u8>) -> Result<()> {
        self.tx
            .send(Command::Rpc { rpc_id, payload })
            .await
            .map_err(|_| RaknetError::NotConnected)
    }
    /// Graceful disconnect (sends `DISCONNECTION_NOTIFICATION`, then closes).
    pub async fn disconnect(&self) -> Result<()> {
        self.tx
            .send(Command::Disconnect)
            .await
            .map_err(|_| RaknetError::NotConnected)
    }
}

enum Command {
    Send {
        data: Vec<u8>,
        reliability: Reliability,
        channel: u8,
    },
    Rpc {
        rpc_id: u8,
        payload: Vec<u8>,
    },
    Disconnect,
}

/// The async transport actor: owns the `UdpSocket`, runs the cipher + reliability + handshake.
pub struct RakPeer {
    _private: (),
}

impl RakPeer {
    /// Bind a local socket, spawn the transport task, and begin the handshake to `server`.
    /// Returns a [`RakHandle`] for sending and a receiver of [`RakEvent`]s.
    pub async fn connect(
        server: SocketAddr,
        config: RakConfig,
    ) -> Result<(RakHandle, mpsc::Receiver<RakEvent>)> {
        transport::connect(server, config).await
    }
}

#[cfg(test)]
mod cipher_tests {
    use super::cipher::{decrypt, encrypt};

    #[test]
    fn golden_vector_pins_extracted_table() {
        // plaintext + port 7777 -> exact ciphertext, computed from the bytes dumped out of
        // g_SampCipherTable @0x499C60 / g_SampChecksumMask @0x4A11E0. If the table is ever
        // mis-transcribed, this vector breaks.
        let plaintext = [
            0x00u8, 0x01, 0x02, 0x7F, 0x80, 0xFF, 0x10, 0x20, 0x33, 0xAB, 0xCD, 0xEF, 0x55, 0xAA,
            0x42, 0x99, 0x01, 0x02, 0x03, 0x04,
        ];
        let expected = [
            0xA8u8, 0x27, 0xC4, 0xFD, 0xE9, 0x75, 0x0A, 0x76, 0xB5, 0x8B, 0xBF, 0x3B, 0x74, 0xB0,
            0xFC, 0xA5, 0x5E, 0x69, 0x50, 0x87, 0xCD,
        ];
        assert_eq!(encrypt(&plaintext, 7777), expected);
        assert_eq!(decrypt(&expected, 7777).unwrap(), plaintext);
    }

    #[test]
    fn round_trip_arbitrary_data_and_ports() {
        for port in [0u16, 1, 0xCC, 7777, 0x1234, 0xFFFF] {
            for len in [0usize, 1, 2, 31, 32, 33, 200] {
                let data: Vec<u8> = (0..len)
                    .map(|i| (i.wrapping_mul(37) ^ 0x5A) as u8)
                    .collect();
                let ct = encrypt(&data, port);
                assert_eq!(ct.len(), data.len() + 1);
                assert_eq!(decrypt(&ct, port).unwrap(), data);
            }
        }
    }

    #[test]
    fn decrypt_rejects_empty_and_corrupt() {
        assert!(decrypt(&[], 7777).is_err());
        let mut ct = encrypt(&[1, 2, 3, 4], 7777);
        ct[0] ^= 0xFF; // corrupt the checksum
        assert!(decrypt(&ct, 7777).is_err());
    }

    #[test]
    fn substitution_table_is_a_permutation() {
        use crate::tables::{INVERSE, SUBSTITUTION};
        for b in 0u16..=255 {
            assert_eq!(INVERSE[SUBSTITUTION[b as usize] as usize], b as u8);
        }
    }
}
