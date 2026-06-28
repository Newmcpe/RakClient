//! The scripted server-side state machine driven by [`crate::MockSampServer`].
//!
//! Framing is delegated to [`raknet::wire`]: the mock owns one [`ReliabilityLayer`] for its single
//! client and exchanges byte-identical datagrams with the real transport. The offline
//! open-connection probe is unframed (handled before the reliability layer); every other message is
//! reliability-framed, so inbound datagrams go through [`ReliabilityLayer::on_receive`] and replies
//! are enqueued and flushed through [`ReliabilityLayer::update`].

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use raknet::wire::ReliabilityLayer;
use raknet::Reliability;
use samp_proto::{ClassId, PlayerId, RpcId, ServerCookie, SyncPacketId};
use tokio::net::UdpSocket;

use crate::wire::{self, Inbound};
use crate::MockFaults;

/// Deterministic player id the mock assigns to every client.
pub const MOCK_PLAYER_ID: PlayerId = PlayerId(7);
/// Deterministic server cookie used to derive the join challenge.
pub const MOCK_SERVER_COOKIE: ServerCookie = ServerCookie(0x5AC0_FFEE);
/// Host name reported in `InitGame`.
pub const MOCK_HOST_NAME: &str = "RakClient Mock Server";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    AwaitOpenConnection,
    AwaitConnectionRequest,
    AwaitClientJoin,
    AwaitRequestClass,
    Playing,
    Closed,
}

/// Run the mock until the socket errors or the task is aborted (on drop).
pub async fn run(socket: UdpSocket, port: u16, faults: MockFaults, sync_count: Arc<AtomicUsize>) {
    let mut phase = Phase::AwaitOpenConnection;
    let mut rel = ReliabilityLayer::new();
    let mut received = 0u32;
    let mut buf = vec![0u8; 2048];

    loop {
        let (len, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(_) => return,
        };

        received = received.wrapping_add(1);
        if should_drop(&faults, received) {
            continue;
        }

        // The client byte-ciphers every outbound datagram, so decryption succeeds for both the
        // offline open-connection probe and connected traffic; they are told apart by content. The
        // mock answers offline messages in the clear, as a real server does.
        let inner = match wire::decrypt(&buf[..len], port) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };

        if inner.first() == Some(&raknet::wire::ID_OPEN_CONNECTION_REQUEST) && inner.len() == 3 {
            rel = ReliabilityLayer::new();
            phase = Phase::AwaitConnectionRequest;
            reply_plaintext(&socket, peer, &faults, &wire::open_connection_reply()).await;
            continue;
        }

        let now = Instant::now();
        let delivered = match rel.on_receive(&inner, now) {
            Ok(messages) => messages,
            Err(_) => continue,
        };
        for message in delivered {
            if let Some(parsed) = wire::parse(&message) {
                phase = advance(phase, parsed, &mut rel, &faults, &sync_count);
            }
        }

        flush(&mut rel, &socket, peer, port, &faults, now).await;
        if phase == Phase::Closed {
            return;
        }
    }
}

fn should_drop(faults: &MockFaults, received: u32) -> bool {
    match faults.drop_every_nth_datagram {
        Some(n) if n != 0 => received.is_multiple_of(n),
        _ => false,
    }
}

/// Advance the scripted state machine, enqueuing any reply into the shared reliability layer.
fn advance(
    phase: Phase,
    message: Inbound,
    rel: &mut ReliabilityLayer,
    faults: &MockFaults,
    sync_count: &Arc<AtomicUsize>,
) -> Phase {
    match (phase, &message) {
        // Tolerate a client that opens straight with the connection request.
        (
            Phase::AwaitOpenConnection | Phase::AwaitConnectionRequest,
            Inbound::ConnectionRequest,
        ) => {
            if faults.reject_connection {
                rel.enqueue(&wire::rejection(), Reliability::Reliable, 0);
                return Phase::Closed;
            }
            let accept = wire::connection_request_accepted(MOCK_PLAYER_ID, MOCK_SERVER_COOKIE);
            rel.enqueue(&accept, Reliability::Reliable, 0);
            Phase::AwaitClientJoin
        }
        (Phase::AwaitClientJoin, Inbound::Rpc { id, .. }) if *id == RpcId::ClientJoin as u8 => {
            let payload = wire::init_game_payload(MOCK_PLAYER_ID, MOCK_HOST_NAME);
            rel.enqueue(
                &wire::rpc(RpcId::InitGame, &payload),
                Reliability::ReliableOrdered,
                0,
            );
            Phase::AwaitRequestClass
        }
        (Phase::AwaitRequestClass, Inbound::Rpc { id, .. }) if *id == RpcId::RequestClass as u8 => {
            let payload = wire::request_class_response_payload(ClassId::default());
            rel.enqueue(
                &wire::rpc(RpcId::RequestClass, &payload),
                Reliability::ReliableOrdered,
                0,
            );
            // The client no longer sends `RequestSpawn` (the spawn is server-driven now): push
            // `RequestSpawnResponse(allow==2)` proactively so the driver's `on_spawn_response`
            // fallback spawns it, instead of waiting for a `RequestSpawn` that never arrives.
            let spawn = wire::request_spawn_response_payload();
            rel.enqueue(
                &wire::rpc(RpcId::RequestSpawn, &spawn),
                Reliability::ReliableOrdered,
                0,
            );
            Phase::Playing
        }
        (Phase::Playing, Inbound::Sync { id }) if *id == SyncPacketId::PlayerSync as u8 => {
            sync_count.fetch_add(1, Ordering::Relaxed);
            Phase::Playing
        }
        (_, Inbound::Disconnect) => Phase::Closed,
        _ => phase,
    }
}

/// Send an offline-handshake datagram in the clear (offline messages are unciphered).
async fn reply_plaintext(socket: &UdpSocket, peer: SocketAddr, faults: &MockFaults, inner: &[u8]) {
    if faults.silent {
        return;
    }
    let _ = socket.send_to(inner, peer).await;
}

/// Flush the reliability layer: pending ACK/NAK ranges, enqueued replies, and due resends.
async fn flush(
    rel: &mut ReliabilityLayer,
    socket: &UdpSocket,
    peer: SocketAddr,
    port: u16,
    faults: &MockFaults,
    now: Instant,
) {
    if faults.silent {
        return;
    }
    let _ = port;
    for datagram in rel.update(now) {
        // With omp encryption disabled the server replies in the clear; only inbound client
        // datagrams are ciphered.
        let _ = socket.send_to(&datagram, peer).await;
    }
}
