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
    build_connected_pong, build_rpc, parse_rpc, ID_CONNECTION_REQUEST, ID_INTERNAL_PING,
    ID_OPEN_CONNECTION_COOKIE, ID_OPEN_CONNECTION_REPLY, ID_OPEN_CONNECTION_REQUEST, ID_PONG,
    ID_RPC, ID_TIMESTAMP,
};
use crate::{
    cipher, Command, DisconnectReason, RakConfig, RakEvent, RakHandle, Reliability, Result,
};

const TICK: Duration = Duration::from_millis(20);
const HANDSHAKE_RETRY: Duration = Duration::from_millis(500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPALIVE: Duration = Duration::from_secs(8);
/// Once connected, if nothing arrives on the RakNet stream for this long the peer is treated as gone
/// even without a disconnect notification — a silently dropped or frozen link. We keepalive every
/// [`KEEPALIVE`] (8 s) and the server streams pongs/ACKs/sync constantly, so ~2 missed cycles of total
/// silence means dead; tear down so the driver reconnects instead of sending sync into the void.
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
/// Receive buffer sized for the largest possible UDP datagram (65507 B payload). Oversized reads on
/// Windows fail the recv with `WSAEMSGSIZE` rather than truncating like Linux, so we never want a
/// datagram to exceed this.
const RECV_BUF: usize = 65535;

/// Some SA-MP hosts (e.g. Arizona) run an anti-DDoS filter that drops connect/game datagrams from
/// any source IP that has not recently sent a valid `SAMP …i` info query. Sending the query first
/// whitelists us for a short window; we re-send it on this interval to stay whitelisted across the
/// handshake and the connected session.
const QUERY_PING_INTERVAL: Duration = Duration::from_secs(5);

/// How long to wait for the server's `SAMP` reply (which confirms the anti-DDoS filter has
/// whitelisted our source IP) before sending the open-connection request anyway. Normally the reply
/// arrives first and triggers the open request immediately; this is just a fallback for a dropped or
/// suppressed reply so a connect never stalls forever.
const WHITELIST_FALLBACK: Duration = Duration::from_millis(400);

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
    // Direct: bind + connect the socket, best-effort anti-DDoS whitelist. Proxied: hunt for an exit
    // that both whitelists on the server (HTTP :80) and relays the game UDP, rotating the session id.
    let (socket, tunnel) = match &config.proxy {
        Some(proxy) => establish_proxied(server, proxy).await?,
        None => {
            let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).await?;
            if let SocketAddr::V4(v4) = server {
                let _ = tokio::time::timeout(
                    Duration::from_secs(6),
                    crate::socks5::http_whitelist(*v4.ip(), None),
                )
                .await;
            }
            socket.connect(server).await?;
            (socket, None)
        }
    };

    let (cmd_tx, cmd_rx) = mpsc::channel(128);
    let (evt_tx, evt_rx) = mpsc::channel(128);

    let task = PeerTask::new(socket, server, config, cmd_rx, evt_tx, tunnel);
    tokio::spawn(task.run());

    Ok((RakHandle { tx: cmd_tx }, evt_rx))
}

/// Max session rotations while hunting for a proxy exit that actually relays the game UDP.
const MAX_PROXY_ROTATIONS: usize = 15;

async fn establish_proxied(
    server: SocketAddr,
    proxy: &crate::socks5::ProxyConfig,
) -> Result<(UdpSocket, Option<Socks5Tunnel>)> {
    let rotatable = proxy.username.contains("-session-");
    let attempts = if rotatable { MAX_PROXY_ROTATIONS } else { 1 };
    let mut last_split: Option<(std::net::Ipv4Addr, std::net::Ipv4Addr)> = None;
    for attempt in 0..attempts {
        let username = if rotatable {
            crate::socks5::rotate_session(&proxy.username, 86_400)
        } else {
            proxy.username.clone()
        };
        let cfg = crate::socks5::ProxyConfig {
            addr: proxy.addr,
            username,
            password: proxy.password.clone(),
        };
        match probe_exit(server, &cfg).await {
            Ok((socket, tunnel)) => {
                tracing::info!(attempt, relay = %tunnel.relay, "proxy exit relays game UDP — using it");
                return Ok((socket, Some(tunnel)));
            }
            Err(ProbeFail::SplitExit { tcp, udp }) => {
                last_split = Some((tcp, udp));
                tracing::warn!(
                    attempt, %tcp, %udp,
                    "proxy exit egresses TCP and UDP from different IPs — the whitelist covers the TCP \
                     IP but game UDP would arrive from the UDP IP; rotating"
                );
            }
            Err(ProbeFail::NoUdpRelay) => {
                if rotatable {
                    tracing::warn!(
                        attempt,
                        "proxy exit did not relay game UDP; rotating session"
                    );
                }
            }
        }
    }
    // A split exit is a property of the provider, not the individual exit, so if we saw one it's the
    // real reason every rotation failed — say so explicitly instead of the generic message.
    let msg = match last_split {
        Some((tcp, udp)) => format!(
            "proxy egresses TCP ({tcp}) and UDP ({udp}) from different IPs — incompatible with \
             source-IP-whitelisting servers (e.g. Arizona); use a single-IP proxy (datacenter/ISP) \
             that relays UDP"
        ),
        None => "no proxy exit relayed the game UDP after rotating sessions".to_string(),
    };
    Err(crate::RaknetError::Proxy(msg))
}

/// Why a proxy-exit probe was rejected, so the caller can give a precise final error instead of a
/// generic "no exit worked".
enum ProbeFail {
    /// The exit egresses TCP and UDP from different IPs — the whitelisted (TCP) IP is not the one the
    /// game UDP arrives from, so a source-IP-whitelisting server silently drops us. Rotating cannot
    /// fix this on such a provider (e.g. SOAX residential).
    SplitExit {
        tcp: std::net::Ipv4Addr,
        udp: std::net::Ipv4Addr,
    },
    /// The exit did not relay the game UDP at all (no query reply), or the association/whitelist
    /// failed. Rotating to a fresh exit may help.
    NoUdpRelay,
}

async fn probe_exit(
    server: SocketAddr,
    cfg: &crate::socks5::ProxyConfig,
) -> std::result::Result<(UdpSocket, Socks5Tunnel), ProbeFail> {
    let SocketAddr::V4(v4) = server else {
        return Err(ProbeFail::NoUdpRelay);
    };
    let assoc = crate::socks5::udp_associate(cfg)
        .await
        .map_err(|_| ProbeFail::NoUdpRelay)?;
    let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|_| ProbeFail::NoUdpRelay)?;

    // Before whitelisting, verify the exit egresses UDP and TCP from the *same* IP. The whitelist
    // (HTTP :80) rides the TCP exit, but the game runs on UDP — if the provider relays UDP from a
    // different IP (e.g. SOAX residential), the server drops our packets no matter how many sessions
    // we rotate. Best-effort: only reject on a *confirmed* mismatch; an inconclusive probe falls
    // through to the UDP query below, which stays the ultimate pass/fail.
    let udp_ip = crate::socks5::stun_exit_ip(&socket, assoc.relay).await;
    let tcp_ip = crate::socks5::tcp_exit_ip(cfg).await;
    if let (Some(udp), Some(tcp)) = (udp_ip, tcp_ip) {
        if udp != tcp {
            return Err(ProbeFail::SplitExit { tcp, udp });
        }
        tracing::debug!(exit = %tcp, "proxy TCP+UDP share one exit IP");
    }

    // Whitelist the exit IP (best-effort; the UDP probe below is the real pass/fail).
    let _ = tokio::time::timeout(
        Duration::from_secs(6),
        crate::socks5::http_whitelist(*v4.ip(), Some(cfg)),
    )
    .await;
    let mut query = Vec::from(*b"SAMP");
    query.extend_from_slice(&v4.ip().octets());
    query.extend_from_slice(&v4.port().to_le_bytes());
    query.push(b'i');
    let wrapped = crate::socks5::wrap_udp(server, &query);
    for _ in 0..4 {
        let _ = socket.send_to(&wrapped, assoc.relay).await;
    }
    let mut buf = [0u8; 512];
    match tokio::time::timeout(Duration::from_secs(4), socket.recv_from(&mut buf)).await {
        Ok(Ok((n, from))) if n > 0 && from.ip() == assoc.relay.ip() => Ok((
            socket,
            Socks5Tunnel {
                dst: server,
                relay: assoc.relay,
                _control: assoc.control,
            },
        )),
        _ => Err(ProbeFail::NoUdpRelay),
    }
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
    /// Task start, used as the epoch for the millisecond clock echoed in `ID_CONNECTED_PONG`.
    started: Instant,
    last_handshake: Instant,
    last_keepalive: Instant,
    last_query_ping: Instant,
    /// When we last received any real RakNet datagram from the peer (excludes `SAMP` query pongs).
    /// Drives the connected-phase dead-peer timeout (see [`PEER_TIMEOUT`]).
    last_recv: Instant,
    /// Whether the open-connection request has been sent yet. Held back until the server's `SAMP`
    /// query reply confirms our IP is whitelisted through the anti-DDoS filter (or the fallback).
    open_sent: bool,
    /// `Some` when tunnelling through a SOCKS5 proxy; `None` = direct connection.
    tunnel: Option<Socks5Tunnel>,
    /// `Some` when `--pcap` is set: records every datagram (both directions) to a libpcap file.
    pcap: Option<crate::pcap::PcapWriter>,
}

/// An established SOCKS5 UDP association the peer tunnels through.
struct Socks5Tunnel {
    /// The real game-server address the SOCKS5 UDP header addresses each datagram to.
    dst: SocketAddr,
    /// The proxy's UDP relay address to `send_to` (proxy mode uses an *unconnected* socket, since a
    /// relay may reply from a different source port than `BND.PORT` — a connected socket would drop
    /// those). Replies are accepted from the relay's IP on any port.
    relay: SocketAddr,
    /// The SOCKS5 control connection, held open so the proxy keeps the UDP association alive.
    _control: tokio::net::TcpStream,
}

impl PeerTask {
    fn new(
        socket: UdpSocket,
        server: SocketAddr,
        config: RakConfig,
        cmd_rx: mpsc::Receiver<Command>,
        evt_tx: mpsc::Sender<RakEvent>,
        tunnel: Option<Socks5Tunnel>,
    ) -> Self {
        let now = Instant::now();
        let pcap = config.pcap.as_ref().and_then(|path| {
            match crate::pcap::PcapWriter::create(path, server) {
                Ok(w) => {
                    tracing::info!(path = %path.display(), "capturing datagrams to pcap");
                    Some(w)
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "could not open pcap file");
                    None
                }
            }
        });
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
            started: now,
            last_handshake: now,
            last_keepalive: now,
            last_query_ping: now - QUERY_PING_INTERVAL,
            last_recv: now,
            open_sent: false,
            tunnel,
            pcap,
        }
    }

    /// Send a datagram to the wire: directly (connected socket) when not proxying, or wrapped in the
    /// SOCKS5 UDP header (addressed to the real server) and `send_to` the proxy relay when tunnelling.
    async fn udp_send(&self, bytes: &[u8]) {
        if let Some(pcap) = &self.pcap {
            pcap.record(true, bytes); // outbound: client → server (byte-ciphered once connected)
        }
        match &self.tunnel {
            Some(tunnel) => {
                let wrapped = crate::socks5::wrap_udp(tunnel.dst, bytes);
                let _ = self.socket.send_to(&wrapped, tunnel.relay).await;
            }
            None => {
                let _ = self.socket.send(bytes).await;
            }
        }
    }

    async fn run(mut self) {
        let mut buf = vec![0u8; RECV_BUF];
        let mut ticker = interval(TICK);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // Ping first, connect second: send the anti-DDoS whitelist query and wait for the server's
        // `SAMP` reply (or the fallback) before the open-connection request — sending it before the
        // filter has whitelisted our IP gets the whole connect handshake silently dropped.
        self.send_query_ping().await;

        loop {
            tokio::select! {
                res = self.socket.recv_from(&mut buf) => {
                    match res {
                        Ok((n, from)) => {
                            // Proxied: accept replies from the relay IP (any port — relays reply from
                            // a port other than BND.PORT), then strip the SOCKS5 UDP header. Direct:
                            // take the datagram as-is.
                            let datagram = match &self.tunnel {
                                Some(tunnel) => {
                                    if from.ip() != tunnel.relay.ip() {
                                        continue;
                                    }
                                    match crate::socks5::unwrap_udp(&buf[..n]) {
                                        Some(inner) => inner.to_vec(),
                                        None => continue,
                                    }
                                }
                                None => buf[..n].to_vec(),
                            };
                            if let Some(pcap) = &self.pcap {
                                pcap.record(false, &datagram); // inbound: server → client (plaintext)
                            }
                            if !self.on_datagram(&datagram).await {
                                break;
                            }
                        }
                        Err(error) => {
                            // Windows surfaces a prior send's ICMP "port unreachable" as
                            // WSAECONNRESET (10054) and an oversized datagram as WSAEMSGSIZE (10040)
                            // on the *next* recv — neither means the session is gone. Skip the read
                            // and keep listening instead of tearing down (see tokio#2017).
                            let transient = matches!(
                                error.kind(),
                                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::WouldBlock
                            ) || error.raw_os_error() == Some(10040);
                            if transient {
                                tracing::debug!(%error, "ignoring transient socket error");
                                continue;
                            }
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
        self.udp_send(&encrypted).await;
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
        self.udp_send(&query).await;
    }

    /// Connected datagrams are byte-ciphered (keyed on the server port).
    async fn send_datagram(&self, plaintext: &[u8]) {
        tracing::trace!(dir = "out", kind = "framed", bytes = %hex(plaintext), "send");
        let encrypted = cipher::encrypt(plaintext, self.cipher_port);
        self.udp_send(&encrypted).await;
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
        self.open_sent = true;
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
            // Fallback: if the server never replied to our whitelist query, start the handshake
            // anyway once the grace window elapses so a suppressed reply doesn't stall the connect.
            if !self.open_sent && now.duration_since(self.last_query_ping) >= WHITELIST_FALLBACK {
                self.last_handshake = now;
                self.send_open_request().await;
            } else if self.open_sent && now.duration_since(self.last_handshake) >= HANDSHAKE_RETRY {
                self.last_handshake = now;
                if self.phase == Phase::OpenRequest {
                    self.send_open_request().await;
                }
            }
        } else {
            // Connected: detect a silently dropped / frozen peer. If nothing has arrived on the RakNet
            // stream for PEER_TIMEOUT, the link is dead even though no disconnect notification came —
            // tear down so the driver reconnects instead of pumping sync into the void forever.
            let silent = now.duration_since(self.last_recv);
            if silent >= PEER_TIMEOUT {
                tracing::warn!(
                    secs = silent.as_secs(),
                    "peer silent past timeout — disconnecting"
                );
                self.emit(RakEvent::Disconnected(DisconnectReason::Timeout))
                    .await;
                return false;
            }
            if now.duration_since(self.last_keepalive) >= KEEPALIVE {
                self.last_keepalive = now;
                self.rel
                    .enqueue(&[ID_TIMESTAMP], Reliability::Unreliable, 0);
            }
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
            // Reply to our own anti-DDoS whitelist query: our IP is now whitelisted, so it's safe to
            // start the connect handshake. Not part of the RakNet stream otherwise.
            if !self.open_sent {
                self.send_open_request().await;
            }
            return true;
        }
        // Any real RakNet datagram is a life sign from the peer — reset the dead-peer timeout. (SAMP
        // query pongs above are excluded, so a server whose game stream froze but whose query port
        // still answers is still detected as dead.)
        self.last_recv = Instant::now();
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
        // Don't flush here: a real RakNet client batches its pending ACK over its periodic update
        // tick rather than sending one immediately per received datagram. During a burst (hundreds
        // of world-init RPCs arrive within ~1s of joining) an eager per-datagram flush fires a
        // separate outbound ACK UDP packet for literally every inbound one — a traffic pattern no
        // genuine client produces. `on_tick` (every `TICK`) already flushes the accumulated ack
        // queue, so leave it to batch naturally.
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
            // Answer the server's connected-ping so it can measure our ping. RakNet handles this
            // internally; our hand-rolled peer must replicate it or the server's ping stat stays
            // 0xFFFF and the anti-cheat flags an unanswered client function. Enqueued unreliably on
            // channel 0 (matching `RakPeer::RunUpdateCycle`); the next `on_tick` flush sends it.
            ID_INTERNAL_PING => {
                let local_ms = self.started.elapsed().as_millis() as u32;
                if let Some(pong) = build_connected_pong(&message, local_ms) {
                    self.rel.enqueue(&pong, Reliability::Unreliable, 0);
                }
                true
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

    /// Build a connected `PeerTask` on a throwaway loopback socket for the dead-peer tests. Sends go
    /// nowhere (the socket is unconnected), which is fine — these only exercise `on_tick`'s timeout.
    /// The command sender is returned so the caller keeps it alive (the task holds the receiver).
    async fn connected_task() -> (PeerTask, mpsc::Sender<Command>, mpsc::Receiver<RakEvent>) {
        let socket = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let server: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let (evt_tx, evt_rx) = mpsc::channel::<RakEvent>(4);
        let mut task = PeerTask::new(
            socket,
            server,
            RakConfig {
                password: None,
                static_data: Vec::new(),
                proxy: None,
                pcap: None,
            },
            cmd_rx,
            evt_tx,
            None,
        );
        task.phase = Phase::Connected;
        (task, cmd_tx, evt_rx)
    }

    #[tokio::test]
    async fn connected_peer_timeout_disconnects_when_silent() {
        // A connected peer that goes silent past PEER_TIMEOUT (no datagrams, not even keepalive pongs)
        // must be torn down with a Timeout so the driver can reconnect instead of sending into a void.
        let (mut task, _cmd_tx, mut evt_rx) = connected_task().await;
        task.last_recv = Instant::now() - PEER_TIMEOUT - Duration::from_secs(1);

        let alive = task.on_tick().await;

        assert!(!alive, "silent peer should stop the task");
        assert!(matches!(
            evt_rx.try_recv(),
            Ok(RakEvent::Disconnected(DisconnectReason::Timeout))
        ));
    }

    #[tokio::test]
    async fn connected_peer_stays_alive_when_recently_heard() {
        let (mut task, _cmd_tx, mut evt_rx) = connected_task().await;
        task.last_recv = Instant::now(); // just heard from the peer

        let alive = task.on_tick().await;

        assert!(alive, "recently-heard peer keeps running");
        assert!(evt_rx.try_recv().is_err(), "no disconnect expected");
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

    /// Investigator's primary suspect: a single inbound datagram can bundle many coalesced
    /// `ReliableOrdered` messages, and `PeerTask::handle_message` → `emit` awaits `evt_tx.send`
    /// once per delivered message. With `evt_tx` bounded (128 in production, here deliberately
    /// tiny), the claim is that a slow consumer makes that `send().await` block, stalling the
    /// `on_datagram` call — and, in `run()`'s `select!`, the whole recv/tick loop — until the
    /// consumer catches up. This isolates exactly that mechanism deterministically (no real
    /// sockets/timers/OS buffers involved) and checks the actual reliability-layer question: once
    /// unblocked, is every event still delivered exactly once, in order? Whether the *server's*
    /// UDP datagrams get silently dropped by the OS while the actor is blocked on `emit` is real
    /// kernel behavior this cannot deterministically reproduce — that half of the suspect is out of
    /// scope for a unit test.
    #[tokio::test]
    async fn evt_channel_backpressure_blocks_recv_but_drains_without_loss() {
        let socket = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let server: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let (evt_tx, mut evt_rx) = mpsc::channel::<RakEvent>(4); // far smaller than the burst
        let mut task = PeerTask::new(
            socket,
            server,
            RakConfig {
                password: None,
                static_data: Vec::new(),
                proxy: None,
                pcap: None,
            },
            cmd_rx,
            evt_tx,
            None,
        );
        task.phase = Phase::Connected;

        // Build a burst of small ReliableOrdered application packets (non-RPC id, so
        // `handle_message` routes them to `RakEvent::Packet`) that coalesce into one datagram,
        // matching "a single inbound datagram can bundle many coalesced RPCs".
        const N: u8 = 50;
        let mut sender = ReliabilityLayer::new();
        for i in 0..N {
            sender.enqueue(&[200, i], Reliability::ReliableOrdered, 0);
        }
        let datagrams = sender.update(Instant::now());
        assert_eq!(
            datagrams.len(),
            1,
            "burst of {N} small packets should coalesce into a single inbound datagram"
        );

        let mut join = tokio::spawn(async move {
            for dg in &datagrams {
                assert!(task.on_datagram(dg).await);
            }
        });

        // The spawned on_datagram call must NOT finish while evt_rx sits undrained: it should be
        // parked inside `emit`'s `evt_tx.send().await` once the 4-slot channel fills.
        tokio::select! {
            _ = &mut join => panic!(
                "on_datagram finished without ever blocking on the bounded evt_tx — the \
                 investigator's backpressure mechanism did not reproduce"
            ),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        let mut received = Vec::new();
        while received.len() < N as usize {
            match evt_rx.recv().await {
                Some(RakEvent::Packet { data }) => received.push(data[1]),
                Some(_) => panic!("unexpected event kind"),
                None => break,
            }
        }
        join.await
            .expect("on_datagram task must complete once the channel is drained");

        assert_eq!(
            received,
            (0..N).collect::<Vec<_>>(),
            "backpressure must delay but never drop or reorder events once drained"
        );
    }
}
