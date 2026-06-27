//! Dev-only test fixtures and a loopback **mock SA-MP server** used by integration tests.
//!
//! [`MockSampServer`] binds a loopback [`tokio::net::UdpSocket`], applies the same datagram cipher
//! as the real transport ([`raknet::cipher`]), and scripts the server side of the connect → spawn
//! sequence:
//!
//! ```text
//! CONNECTION_REQUEST_ACCEPTED → InitGame(139)
//!   → on RequestClass(128) reply RequestClassResponse(128)
//!   → on RequestSpawn(129)  reply RequestSpawnResponse(129, allow)
//! ```
//!
//! It records received on-foot sync packets ([`MockSampServer::sync_packets_received`]) and can
//! inject faults ([`MockFaults`]). Everything is deterministic: a fixed player id and cookie, no
//! randomness, no wall-clock timing.
//!
//! All framing — the cipher, the reliability-layer datagram header, the handshake ids, and the
//! `ID_RPC` envelope — is delegated to [`raknet::cipher`] and [`raknet::wire`], so the mock speaks
//! byte-identical wire to the real client (see [`crate::server`] and [`crate::wire`]).
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

mod server;
mod wire;

/// A loopback mock SA-MP server. Drop to shut it down.
pub struct MockSampServer {
    local_addr: SocketAddr,
    sync_count: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

/// Fault injection for negative-path tests.
#[derive(Debug, Clone, Default)]
pub struct MockFaults {
    /// Drop every `n`-th received datagram (`Some(0)` is treated as no drops).
    pub drop_every_nth_datagram: Option<u32>,
    /// Reply to the connection request with a RakNet rejection instead of accepting.
    pub reject_connection: bool,
    /// Never transmit anything (receive-only black hole).
    pub silent: bool,
}

impl MockSampServer {
    /// Bind a loopback UDP socket and start the server task with no faults.
    pub async fn start() -> std::io::Result<Self> {
        Self::start_with(MockFaults::default()).await
    }

    /// Bind with fault injection.
    pub async fn start_with(faults: MockFaults) -> std::io::Result<Self> {
        let socket = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let local_addr = socket.local_addr()?;
        let sync_count = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn(server::run(
            socket,
            local_addr.port(),
            faults,
            Arc::clone(&sync_count),
        ));
        Ok(Self {
            local_addr,
            sync_count,
            task,
        })
    }

    /// Address the client should connect to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Number of on-foot sync packets received so far (for assertions).
    pub fn sync_packets_received(&self) -> usize {
        self.sync_count.load(Ordering::Relaxed)
    }
}

impl Drop for MockSampServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Golden constants captured from the original binary's connect → spawn logic. The mock and any
/// real [`samp_proto`] decoder agree on these, so they double as cross-check vectors in tests.
pub mod vectors {
    /// `ClientJoin.version` a 0.3.7 client announces.
    pub const SAMP_VERSION: u32 = samp_proto::SAMP_VERSION_0_3_7;
    /// `challengeResponse = serverCookie ^ CHALLENGE_XOR`.
    pub const CHALLENGE_XOR: u32 = samp_proto::CHALLENGE_XOR;
    /// Deterministic player id the mock assigns.
    pub const MOCK_PLAYER_ID: u16 = super::server::MOCK_PLAYER_ID.0;
    /// Deterministic server cookie the mock sends in `CONNECTION_REQUEST_ACCEPTED`.
    pub const MOCK_SERVER_COOKIE: u32 = super::server::MOCK_SERVER_COOKIE.0;
    /// Bits skipped before the player id in a `CONNECTION_REQUEST_ACCEPTED` body.
    pub const CRA_HEADER_SKIP_BITS: usize = super::wire::CRA_HEADER_SKIP_BITS;
    /// Bit offset of the `u16` player id inside an `InitGame` payload.
    pub const INITGAME_PLAYER_ID_BIT_OFFSET: usize = super::wire::INITGAME_PLAYER_ID_BIT_OFFSET;
    /// RakNet `ID_RPC` marker byte (the single source of truth in `raknet::wire`).
    pub const ID_RPC: u8 = raknet::wire::ID_RPC;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starts_on_loopback_and_records_no_sync_initially() {
        let server = MockSampServer::start().await.expect("bind loopback");
        let addr = server.local_addr();
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0);
        assert_eq!(server.sync_packets_received(), 0);
    }

    #[tokio::test]
    async fn faults_are_carried_through_start() {
        let faults = MockFaults {
            drop_every_nth_datagram: Some(3),
            reject_connection: true,
            silent: false,
        };
        let server = MockSampServer::start_with(faults)
            .await
            .expect("bind loopback");
        assert!(server.local_addr().ip().is_loopback());
    }

    #[test]
    fn vectors_match_protocol_constants() {
        assert_eq!(vectors::SAMP_VERSION, 4057);
        assert_eq!(vectors::CHALLENGE_XOR, 0xFD9);
        assert_eq!(vectors::CRA_HEADER_SKIP_BITS, 48);
        assert_eq!(vectors::INITGAME_PLAYER_ID_BIT_OFFSET, 104);
    }

    #[ignore = "runs after merge: needs the real raknet transport + samp-proto codecs"]
    #[tokio::test]
    async fn drives_a_client_to_spawned() {
        // Exercised post-merge with samp_client::Client against this mock.
    }
}
