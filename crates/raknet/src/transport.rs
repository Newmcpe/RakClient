//! Async [`RakPeer`](crate::RakPeer) transport actor: owns the `UdpSocket`, drives the SA-MP /
//! RakNet 3.x ("legacy network", as spoken by open.mp) connection handshake, and pumps the
//! [`crate::reliability`] layer through the [`crate::cipher`].
//!
//! Handshake (reversed from open.mp's `RakPeer`):
//! 1. C→S `[24][0][pad]` (`ID_OPEN_CONNECTION_REQUEST`, exactly 3 bytes).
//! 2. S→C `[26][cookie]` (`ID_OPEN_CONNECTION_COOKIE`) when the anti-flood cookie is enabled; the
//!    client echoes the cookie in a follow-up open-connection request `[24][cookie...]`.
//! 3. S→C `[25][0]` (`ID_OPEN_CONNECTION_REPLY`).
//! 4. C→S reliability-framed `[11][password]` (`ID_CONNECTION_REQUEST`).
//! 5. S→C reliability-framed `[34]…` (`ID_CONNECTION_REQUEST_ACCEPTED`) — connection established.
//!
//! Offline handshake datagrams are plaintext; connected (reliability-framed) datagrams are
//! byte-ciphered, so a datagram that decrypts with a valid checksum is treated as framed and one
//! that does not is treated as a plaintext offline message. Every datagram is logged at `trace`.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};

use crate::reliability::ReliabilityLayer;
use crate::wire::{
    build_rpc, parse_rpc, ID_CONNECTION_REQUEST, ID_OPEN_CONNECTION_COOKIE,
    ID_OPEN_CONNECTION_REPLY, ID_OPEN_CONNECTION_REQUEST, ID_PONG, ID_RPC, ID_TIMESTAMP,
};
use crate::{
    cipher, Command, DisconnectReason, RakConfig, RakEvent, RakHandle, Reliability, Result,
};

const TICK: Duration = Duration::from_millis(20);
const HANDSHAKE_RETRY: Duration = Duration::from_millis(500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPALIVE: Duration = Duration::from_secs(8);
const RECV_BUF: usize = 2048;

/// Some SA-MP hosts (e.g. Arizona) run an anti-DDoS filter that drops connect/game datagrams from
/// any source IP that has not recently sent a valid `SAMP …i` info query. Sending the query first
/// whitelists us for a short window; we re-send it on this interval to stay whitelisted across the
/// handshake and the connected session.
const QUERY_PING_INTERVAL: Duration = Duration::from_secs(5);

/// `SAMP_PETARDED` — XOR mask the client applies to the server cookie in the open-connection
/// request (open.mp `SAMPRakNet::OnConnectionRequest`).
const SAMP_PETARDED: u16 = 0x6969;

const ID_AUTH_KEY: u8 = 12;
const ID_NEW_INCOMING_CONNECTION: u8 = 30;
const ID_CONNECTION_REQUEST_ACCEPTED: u8 = 34;
const ID_DISCONNECTION_NOTIFICATION: u8 = 32;
const ID_CONNECTION_LOST: u8 = 33;
const ID_NO_FREE_INCOMING_CONNECTIONS: u8 = 31;
const ID_CONNECTION_BANNED: u8 = 36;
const ID_INVALID_PASSWORD: u8 = 37;
const ID_CONNECTION_ATTEMPT_FAILED: u8 = 29;

pub(crate) async fn connect(
    server: SocketAddr,
    config: RakConfig,
) -> Result<(RakHandle, mpsc::Receiver<RakEvent>)> {
    let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.connect(server).await?;

    let (cmd_tx, cmd_rx) = mpsc::channel(128);
    let (evt_tx, evt_rx) = mpsc::channel(128);

    let task = PeerTask::new(socket, server, config, cmd_rx, evt_tx);
    tokio::spawn(task.run());

    Ok((RakHandle { tx: cmd_tx }, evt_rx))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Phase {
    OpenRequest,
    ConnectionRequest,
    Connected,
}

struct PeerTask {
    socket: UdpSocket,
    server: SocketAddr,
    cipher_port: u16,
    config: RakConfig,
    cmd_rx: mpsc::Receiver<Command>,
    evt_tx: mpsc::Sender<RakEvent>,
    rel: ReliabilityLayer,
    phase: Phase,
    cookie: Vec<u8>,
    deadline: Instant,
    last_handshake: Instant,
    last_keepalive: Instant,
    last_query_ping: Instant,
}

impl PeerTask {
    fn new(
        socket: UdpSocket,
        server: SocketAddr,
        config: RakConfig,
        cmd_rx: mpsc::Receiver<Command>,
        evt_tx: mpsc::Sender<RakEvent>,
    ) -> Self {
        let now = Instant::now();
        PeerTask {
            socket,
            server,
            cipher_port: server.port(),
            config,
            cmd_rx,
            evt_tx,
            rel: ReliabilityLayer::new(),
            phase: Phase::OpenRequest,
            cookie: Vec::new(),
            deadline: now + CONNECT_TIMEOUT,
            last_handshake: now,
            last_keepalive: now,
            last_query_ping: now - QUERY_PING_INTERVAL,
        }
    }

    async fn run(mut self) {
        let mut buf = vec![0u8; RECV_BUF];
        let mut ticker = interval(TICK);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        self.send_query_ping().await;
        self.send_open_request().await;

        loop {
            tokio::select! {
                res = self.socket.recv(&mut buf) => {
                    match res {
                        Ok(n) => {
                            let datagram = buf[..n].to_vec();
                            if !self.on_datagram(&datagram).await {
                                break;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "socket recv error");
                            let _ = self
                                .emit(RakEvent::Disconnected(DisconnectReason::ConnectionLost))
                                .await;
                            break;
                        }
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            if !self.on_command(cmd).await {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = ticker.tick() => {
                    if !self.on_tick().await {
                        break;
                    }
                }
            }
        }
    }

    async fn emit(&self, event: RakEvent) -> bool {
        self.evt_tx.send(event).await.is_ok()
    }

    /// Outgoing offline handshake datagrams are byte-ciphered too: the server deciphers every
    /// inbound datagram (only its own offline replies go out in the clear).
    async fn send_offline(&self, plaintext: &[u8]) {
        tracing::trace!(dir = "out", kind = "offline", id = plaintext.first().copied(), bytes = %hex(plaintext), "send");
        let encrypted = cipher::encrypt(plaintext, self.cipher_port);
        let _ = self.socket.send(&encrypted).await;
    }

    /// Send a raw (un-ciphered) `SAMP <ip4> <port-le> i` info query. This is what whitelists our
    /// source IP through the server's anti-DDoS filter so the connect handshake is not dropped. The
    /// reply (also starting with `SAMP`) is ignored by [`Self::on_datagram`].
    async fn send_query_ping(&mut self) {
        self.last_query_ping = Instant::now();
        let SocketAddr::V4(v4) = self.server else {
            return;
        };
        let mut query = Vec::with_capacity(11);
        query.extend_from_slice(b"SAMP");
        query.extend_from_slice(&v4.ip().octets());
        query.extend_from_slice(&v4.port().to_le_bytes());
        query.push(b'i');
        let _ = self.socket.send(&query).await;
    }

    /// Connected datagrams are byte-ciphered (keyed on the server port).
    async fn send_datagram(&self, plaintext: &[u8]) {
        tracing::trace!(dir = "out", kind = "framed", bytes = %hex(plaintext), "send");
        let encrypted = cipher::encrypt(plaintext, self.cipher_port);
        let _ = self.socket.send(&encrypted).await;
    }

    async fn flush_reliability(&mut self, now: Instant) {
        for datagram in self.rel.update(now) {
            self.send_datagram(&datagram).await;
        }
    }

    async fn send_open_request(&mut self) {
        self.last_handshake = Instant::now();
        // Always exactly 3 bytes: `[ID_OPEN_CONNECTION_REQUEST][xordCookie: u16 le]`. The server
        // accepts when `xordCookie ^ SAMP_PETARDED == GetCookie(addr)`; before a cookie is issued the
        // pad is zero (which fails and triggers the `[26][cookie]` challenge).
        let mut msg = vec![ID_OPEN_CONNECTION_REQUEST, 0, 0];
        if self.cookie.len() == 2 {
            let cookie = u16::from_le_bytes([self.cookie[0], self.cookie[1]]);
            let xord = cookie ^ SAMP_PETARDED;
            msg[1..3].copy_from_slice(&xord.to_le_bytes());
        }
        self.send_offline(&msg).await;
    }

    async fn send_new_incoming_connection(&mut self) {
        // `[ID_NEW_INCOMING_CONNECTION][server ip: u32 le][server port: u16 le]` (7 bytes).
        let mut msg = vec![ID_NEW_INCOMING_CONNECTION];
        match self.server.ip() {
            std::net::IpAddr::V4(ip) => msg.extend_from_slice(&ip.octets()),
            std::net::IpAddr::V6(_) => msg.extend_from_slice(&[0, 0, 0, 0]),
        }
        msg.extend_from_slice(&self.server.port().to_le_bytes());
        self.rel.enqueue(&msg, Reliability::Reliable, 0);
        let now = Instant::now();
        self.flush_reliability(now).await;
    }

    async fn send_connection_request(&mut self) {
        if self.phase == Phase::Connected {
            return;
        }
        self.phase = Phase::ConnectionRequest;
        self.last_handshake = Instant::now();
        let mut payload = vec![ID_CONNECTION_REQUEST];
        if let Some(pw) = &self.config.password {
            payload.extend_from_slice(pw.as_bytes());
        }
        self.rel.enqueue(&payload, Reliability::Reliable, 0);
        let now = Instant::now();
        self.flush_reliability(now).await;
    }

    async fn on_tick(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_query_ping) >= QUERY_PING_INTERVAL {
            self.send_query_ping().await;
        }
        if self.phase != Phase::Connected {
            if now >= self.deadline {
                tracing::warn!("handshake timed out");
                self.emit(RakEvent::Disconnected(DisconnectReason::Timeout))
                    .await;
                return false;
            }
            if now.duration_since(self.last_handshake) >= HANDSHAKE_RETRY {
                self.last_handshake = now;
                if self.phase == Phase::OpenRequest {
                    self.send_open_request().await;
                }
            }
        } else if now.duration_since(self.last_keepalive) >= KEEPALIVE {
            self.last_keepalive = now;
            self.rel
                .enqueue(&[ID_TIMESTAMP], Reliability::Unreliable, 0);
        }
        self.flush_reliability(now).await;
        true
    }

    async fn on_command(&mut self, cmd: Command) -> bool {
        let now = Instant::now();
        match cmd {
            Command::Send {
                data,
                reliability,
                channel,
            } => {
                self.rel.enqueue(&data, reliability, channel);
                self.flush_reliability(now).await;
                true
            }
            Command::Rpc { rpc_id, payload } => {
                let frame = build_rpc(rpc_id, &payload);
                self.rel.enqueue(&frame, Reliability::ReliableOrdered, 0);
                self.flush_reliability(now).await;
                true
            }
            Command::Disconnect => {
                self.rel
                    .enqueue(&[ID_DISCONNECTION_NOTIFICATION], Reliability::Reliable, 0);
                self.flush_reliability(now).await;
                let _ = self
                    .emit(RakEvent::Disconnected(DisconnectReason::Local))
                    .await;
                false
            }
        }
    }

    /// Returns `false` when the task should stop.
    async fn on_datagram(&mut self, raw: &[u8]) -> bool {
        // With omp encryption disabled the server sends every datagram in the clear (it still
        // deciphers ours). During the handshake the open-connection ids are unambiguous; once
        // connected every datagram is reliability-framed.
        if raw.starts_with(b"SAMP") {
            // Reply to our own anti-DDoS whitelist query; not part of the RakNet stream.
            return true;
        }
        let first = raw.first().copied().unwrap_or(0);
        if self.phase != Phase::Connected && is_offline_id(first) {
            tracing::trace!(dir = "in", kind = "offline", id = first, bytes = %hex(raw), "recv");
            return self.handle_offline(raw).await;
        }

        tracing::trace!(dir = "in", kind = "framed", len = raw.len(), bytes = %hex(raw), "recv");
        let now = Instant::now();
        let delivered = match self.rel.on_receive(raw, now) {
            Ok(d) => d,
            Err(error) => {
                tracing::trace!(?error, "reliability parse failed");
                return true;
            }
        };
        tracing::trace!(
            count = delivered.len(),
            ids = ?delivered.iter().map(|m| m.first().copied()).collect::<Vec<_>>(),
            "delivered"
        );
        for message in delivered {
            if !self.handle_message(message).await {
                return false;
            }
        }
        self.flush_reliability(now).await;
        true
    }

    /// Returns `false` when the task should stop.
    async fn handle_offline(&mut self, raw: &[u8]) -> bool {
        let first = raw.first().copied().unwrap_or(0);
        match first {
            ID_OPEN_CONNECTION_COOKIE => {
                // `[26][cookie...]` — echo the cookie in the next open-connection request.
                self.cookie = raw[1..].to_vec();
                self.send_open_request().await;
                true
            }
            ID_OPEN_CONNECTION_REPLY => {
                self.send_connection_request().await;
                true
            }
            ID_NO_FREE_INCOMING_CONNECTIONS => {
                self.emit(RakEvent::Disconnected(DisconnectReason::ServerFull))
                    .await;
                false
            }
            ID_PONG => true,
            other => {
                if let Some(reason) = disconnect_reason(other) {
                    self.emit(RakEvent::Disconnected(reason)).await;
                    return false;
                }
                true
            }
        }
    }

    /// Answer the server's `ID_AUTH_KEY` challenge (`[12][len][send\0]`) with the paired `recv`
    /// string from the auth table (`[12][recv_len][recv]`).
    async fn handle_auth_key(&mut self, message: &[u8]) {
        let Some(&len) = message.get(1) else { return };
        let send_len = (len as usize).saturating_sub(1);
        let Some(send) = message.get(2..2 + send_len) else {
            return;
        };
        let Ok(send) = std::str::from_utf8(send) else {
            tracing::warn!("auth challenge was not valid utf-8");
            return;
        };
        let Some((_, recv)) = crate::auth_table::AUTH_TABLE
            .iter()
            .find(|(s, _)| *s == send)
        else {
            tracing::warn!(send, "no auth-table entry for server challenge");
            return;
        };
        let mut resp = vec![ID_AUTH_KEY, recv.len() as u8];
        resp.extend_from_slice(recv.as_bytes());
        tracing::debug!(send, recv, "answering ID_AUTH_KEY challenge");
        self.rel.enqueue(&resp, Reliability::Reliable, 0);
    }

    /// Returns `false` when the task should stop.
    async fn handle_message(&mut self, message: Vec<u8>) -> bool {
        let Some(&id) = message.first() else {
            return true;
        };
        if let Some(reason) = disconnect_reason(id) {
            self.emit(RakEvent::Disconnected(reason)).await;
            return false;
        }
        match id {
            ID_CONNECTION_REQUEST_ACCEPTED => {
                if self.phase != Phase::Connected {
                    self.phase = Phase::Connected;
                    self.last_keepalive = Instant::now();
                    tracing::debug!(body = %hex(&message[1..]), "CONNECTION_REQUEST_ACCEPTED");
                    // The server stays in HANDLING_CONNECTION_REQUEST (ignoring RPCs) until it
                    // receives ID_NEW_INCOMING_CONNECTION carrying its own address.
                    self.send_new_incoming_connection().await;
                    return self
                        .emit(RakEvent::Connected {
                            body: message[1..].to_vec(),
                        })
                        .await;
                }
                true
            }
            ID_AUTH_KEY => {
                self.handle_auth_key(&message).await;
                true
            }
            ID_TIMESTAMP | ID_RPC => {
                if let Some((rpc_id, payload)) = parse_rpc(&message) {
                    tracing::trace!(rpc_id, payload_len = payload.len(), "rpc");
                    self.emit(RakEvent::Rpc {
                        id: rpc_id,
                        payload,
                    })
                    .await
                } else {
                    tracing::trace!(bytes = %hex(&message), "rpc parse failed");
                    true
                }
            }
            _ => self.emit(RakEvent::Packet { data: message }).await,
        }
    }
}

fn is_offline_id(id: u8) -> bool {
    matches!(
        id,
        ID_OPEN_CONNECTION_REPLY
            | ID_OPEN_CONNECTION_COOKIE
            | ID_PONG
            | ID_NO_FREE_INCOMING_CONNECTIONS
            | ID_CONNECTION_BANNED
            | ID_CONNECTION_ATTEMPT_FAILED
    )
}

fn disconnect_reason(id: u8) -> Option<DisconnectReason> {
    match id {
        ID_DISCONNECTION_NOTIFICATION => Some(DisconnectReason::ClosedByServer),
        ID_CONNECTION_LOST => Some(DisconnectReason::ConnectionLost),
        ID_NO_FREE_INCOMING_CONNECTIONS => Some(DisconnectReason::ServerFull),
        ID_CONNECTION_BANNED => Some(DisconnectReason::Banned),
        ID_INVALID_PASSWORD => Some(DisconnectReason::InvalidPassword),
        ID_CONNECTION_ATTEMPT_FAILED => Some(DisconnectReason::AttemptFailed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_ids_map_to_reasons() {
        assert_eq!(
            disconnect_reason(ID_NO_FREE_INCOMING_CONNECTIONS),
            Some(DisconnectReason::ServerFull)
        );
        assert_eq!(
            disconnect_reason(ID_INVALID_PASSWORD),
            Some(DisconnectReason::InvalidPassword)
        );
        assert_eq!(disconnect_reason(200), None);
    }

    /// Drives two reliability layers across real loopback `UdpSocket`s through the cipher, with the
    /// datagrams sent in reverse order, and asserts the receiver reorders them.
    #[tokio::test]
    async fn loopback_reliable_ordered_over_udp() {
        let a = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let b = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let a_addr = a.local_addr().unwrap();
        let b_addr = b.local_addr().unwrap();
        a.connect(b_addr).await.unwrap();
        b.connect(a_addr).await.unwrap();
        let port = b_addr.port();

        let mut tx = ReliabilityLayer::new();
        let mut rx = ReliabilityLayer::new();

        let mut datagrams = Vec::new();
        for i in 0u8..5 {
            tx.enqueue(&[i, 0xAA], Reliability::ReliableOrdered, 0);
            datagrams.extend(tx.update(Instant::now()));
        }
        assert_eq!(datagrams.len(), 5);

        for datagram in datagrams.iter().rev() {
            let encrypted = cipher::encrypt(datagram, port);
            a.send(&encrypted).await.unwrap();
        }

        let mut ordered = Vec::new();
        let mut buf = [0u8; RECV_BUF];
        while ordered.len() < 5 {
            let n = tokio::time::timeout(Duration::from_secs(2), b.recv(&mut buf))
                .await
                .expect("recv timed out")
                .unwrap();
            let plain = cipher::decrypt(&buf[..n], port).unwrap();
            for message in rx.on_receive(&plain, Instant::now()).unwrap() {
                ordered.push(message[0]);
            }
        }
        assert_eq!(ordered, vec![0, 1, 2, 3, 4]);
    }
}
