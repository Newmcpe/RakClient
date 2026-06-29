//! The connection state machine.
//!
//! [`Driver`] is generic over a [`Transport`] so it can be unit-tested against a scripted fake while
//! production instantiates it with the real RakNet transport. Packet bodies are encoded/decoded
//! through the [`samp_proto::Encode`]/[`samp_proto::Decode`] traits on each packet struct. It is
//! pumped one event at a time through [`Driver::next_event`], which internally `select!`s over
//! incoming transport events, the on-foot sync interval (while `Spawned`), and the reconnect timer.

use std::collections::VecDeque;
use std::future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use raknet::{DisconnectReason, RakEvent, Reliability};
use samp_proto::{
    ChatMessage, ChatOutgoing, ClientJoin, Decode, Direction, Encode, InitGame, OnFootSync,
    OutboundMsg, PlayerId, Quaternion, RequestClass, RequestClassResponse, RequestSpawn,
    RequestSpawnResponse, RpcId, ServerMessage, Spawn, SpectatorSync, SyncPacketId, Vector3,
    Verdict, WeaponId, CHALLENGE_XOR, SAMP_VERSION_0_3_7,
};
use tokio::time::{self, Interval, MissedTickBehavior, Sleep};

use crate::aim::AimSync;
use crate::client_emulation::ClientEmulation;
use crate::registry::PacketRegistry;
use crate::state::SharedLocalPlayer;
use crate::transport::Transport;
use crate::{ClientConfig, ClientEvent, ConnectionState};

/// How often the registry's `on_update` handlers fire while connected (drives script timers).
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);

/// When the bot's on-foot state is unchanged, resend a keepalive sync at most this often instead of
/// every `sync_interval` tick — a stationary bridge bot barely moves, so this cuts steady-state CPU
/// and bandwidth while still proving liveness to the server.
const IDLE_SYNC_INTERVAL: Duration = Duration::from_secs(1);

const RPC_INIT_GAME: u8 = RpcId::InitGame as u8;
const RPC_REQUEST_CLASS: u8 = RpcId::RequestClass as u8;
const RPC_REQUEST_SPAWN: u8 = RpcId::RequestSpawn as u8;
const RPC_CONNECTION_REJECTED: u8 = RpcId::ConnectionRejected as u8;
const RPC_CLIENT_MESSAGE: u8 = RpcId::ClientMessage as u8;
const RPC_CHAT: u8 = RpcId::Chat as u8;
/// `RPC_ScrToggleSpectating` (124, 0x7C). The server sends `toggle=1` to put us in spectate and
/// `toggle=0` to drop us out — at which point RakSAMP Lite (`sub_45B260`) calls `Net_Spawn`. This is
/// the Arizona post-login spawn trigger.
const RPC_TOGGLE_SPECTATING: u8 = 124;

const SPAWN_HEALTH: u8 = 100;

/// What woke the `select!` in [`Driver::step`].
enum Step {
    Event(Option<RakEvent>),
    SyncTick,
    Reconnect,
    Update,
}

pub(crate) struct Driver<T: Transport> {
    config: ClientConfig,
    transport: T,
    state: ConnectionState,
    pending: VecDeque<ClientEvent>,
    local_id: PlayerId,
    sync: OnFootSync,
    /// The last on-foot sync actually sent, and when — drives adaptive sending (only resend on a
    /// state change, otherwise at the slow idle keepalive cadence).
    last_sync: OnFootSync,
    last_sync_at: Option<Instant>,
    sync_timer: Option<Interval>,
    reconnect_timer: Option<Pin<Box<Sleep>>>,
    closed: bool,
    /// Packet-handler registry: scripts/observers intercept RPCs here before the FSM sees them.
    registry: PacketRegistry,
    /// Fires the registry's `on_update` handlers; armed only when handlers are registered.
    update_timer: Option<Interval>,
    /// Shared local-player state mirrored to/from `sync`, exposing `getBot*`/`setBot*` to scripts.
    bot_state: Option<SharedLocalPlayer>,
    /// Native aim-sync emulation (always on — standard client behaviour).
    aim: AimSync,
    /// Standard-client emulation: ClientCheck answers, weapon inventory, score-ping, vehicle
    /// ownership (always on; acts once a shared bot state is attached).
    emulation: ClientEmulation,
    /// True between join and spawn: we spectate (sending spectator sync) until the server drives the
    /// spawn via `RequestSpawnResponse(allow==2)`, or the script calls `sampSpawnPlayer()`.
    spectating: bool,
    /// Mirrors RakSAMP Lite's `g_bSpawnRequested`: set when we send `RequestSpawn` (via
    /// `sampSpawnPlayer()`), so a non-2 `RequestSpawnResponse` allow still spawns us.
    spawn_requested: bool,
    /// Mirrors RakSAMP Lite's `g_bSpectating`: the last `TogglePlayerSpectating` value from the server.
    /// A `1 → 0` transition is the server dropping us out of spectate → spawn (the Arizona trigger).
    server_spectating: bool,
    /// Set when a login `DialogResponse` is sent while spectating; the pump loop then spawns. This is
    /// the order the real client uses (login dialog answered → spawn), deferred to avoid recursing
    /// through `send_rpc`.
    spawn_after_login: bool,
}

impl<T: Transport> Driver<T> {
    pub(crate) fn new(config: ClientConfig, transport: T) -> Self {
        let mut pending = VecDeque::new();
        pending.push_back(ClientEvent::StateChanged(ConnectionState::Connecting));
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x1234_5678);
        let aim = AimSync::new(seed);
        let emulation = ClientEmulation::new(seed ^ 0x5555_5555_5555_5555, Instant::now());
        Self {
            config,
            transport,
            state: ConnectionState::Connecting,
            pending,
            local_id: PlayerId::default(),
            sync: OnFootSync::default(),
            last_sync: OnFootSync::default(),
            last_sync_at: None,
            sync_timer: None,
            reconnect_timer: None,
            closed: false,
            registry: PacketRegistry::new(),
            update_timer: None,
            bot_state: None,
            aim,
            emulation,
            spectating: false,
            spawn_requested: false,
            server_spectating: false,
            spawn_after_login: false,
        }
    }

    /// Share a [`SharedLocalPlayer`] with the script engine: the driver mirrors `sync` into it and
    /// reads `setBot*` writes back out of it.
    pub(crate) fn with_bot_state(mut self, state: SharedLocalPlayer) -> Self {
        self.bot_state = Some(state);
        self
    }

    /// Attach a packet-handler registry. Arms the `on_update` timer if the registry registered any
    /// periodic handlers.
    pub(crate) fn with_registry(mut self, registry: PacketRegistry) -> Self {
        if registry.wants_update() {
            let mut interval = time::interval(UPDATE_INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            self.update_timer = Some(interval);
        }
        self.registry = registry;
        self
    }

    pub(crate) fn state(&self) -> &ConnectionState {
        &self.state
    }

    #[cfg(test)]
    pub(crate) fn reconnect_scheduled(&self) -> bool {
        self.reconnect_timer.is_some()
    }

    pub(crate) async fn disconnect(&mut self) -> raknet::Result<()> {
        self.sync_timer = None;
        self.reconnect_timer = None;
        self.closed = true;
        self.transport.disconnect().await
    }

    pub(crate) async fn next_event(&mut self) -> Option<ClientEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.closed && self.reconnect_timer.is_none() {
                return None;
            }
            match self.step().await {
                Step::Event(Some(event)) => self.on_rak_event(event).await,
                Step::Event(None) => self.on_transport_closed(),
                Step::SyncTick => self.on_sync_tick(false).await,
                Step::Reconnect => self.on_reconnect().await,
                Step::Update => self.registry.tick(),
            }
            self.flush_outbox().await;
            self.poll_bot_actions().await;
            if self.spawn_after_login && !matches!(self.state, ConnectionState::Spawned) {
                self.spawn_after_login = false;
                self.spectating = false;
                self.enter_spawned().await;
            }
        }
    }

    /// Send everything scripts queued via `sampSendPacket`/`sampSendRpc` since the last drain, each
    /// with the reliability/channel the script asked for (`sendPacket()` defaults to
    /// reliable-ordered on channel 0 — the Arizona `220` path).
    async fn flush_outbox(&mut self) {
        for msg in self.registry.drain_outbox() {
            let result = match msg {
                OutboundMsg::Packet {
                    data,
                    reliability,
                    channel,
                } => {
                    // Outgoing packets pass through the `onSendPacket` chokepoint first.
                    let id = data.first().copied().unwrap_or(0);
                    let data = match self
                        .registry
                        .dispatch_packet(Direction::Outgoing, id, &data)
                    {
                        Verdict::Drop => continue,
                        Verdict::Pass => data,
                        Verdict::Rewrite(bytes) => bytes,
                    };
                    if id == 220 {
                        tracing::debug!(
                            sub_id = data.get(1).copied().unwrap_or(0),
                            channel,
                            reliability,
                            payload = %data.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                            "outbound 220",
                        );
                    }
                    self.transport
                        .send(data, reliability_from_wire(reliability), channel)
                        .await
                }
                OutboundMsg::Rpc { id, payload } => {
                    tracing::debug!(
                        rpc_id = id,
                        payload = %payload.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                        "outbound RPC",
                    );
                    // A script answering the login dialog (`sampSendDialogResponse`) while we spectate
                    // → spawn next, the order the real client uses (login answered → spawn).
                    if self.spectating
                        && id == RpcId::DialogResponse as u8
                        && !matches!(self.state, ConnectionState::Spawned)
                    {
                        self.spawn_after_login = true;
                    }
                    self.transport.rpc(id, payload).await
                }
            };
            if let Err(error) = result {
                tracing::warn!(%error, "failed to send script-queued message");
                self.on_disconnect(DisconnectReason::ConnectionLost);
                return;
            }
        }
    }

    /// Push the authoritative `sync` fields into the shared local-player state (so `getBot*` reflects
    /// what the server set, e.g. the spawn position).
    fn mirror_to_state(&self) {
        if let Some(state) = &self.bot_state {
            let mut s = state.borrow_mut();
            s.on_foot.position = self.sync.position;
            s.on_foot.quaternion = self.sync.quaternion;
            s.on_foot.health = self.sync.health;
            s.on_foot.armour = self.sync.armour;
            s.on_foot.weapon = self.sync.weapon.0;
        }
    }

    /// Pull script `setBot*` writes back into `sync` before sending it.
    fn mirror_from_state(&mut self) {
        if let Some(state) = &self.bot_state {
            let s = state.borrow();
            self.sync.position = s.on_foot.position;
            self.sync.quaternion = s.on_foot.quaternion;
            self.sync.health = s.on_foot.health;
            self.sync.armour = s.on_foot.armour;
            self.sync.weapon = WeaponId(s.on_foot.weapon);
        }
    }

    /// Mirror the native aim state into the shared local-player model so `LocalPlayer.aim` reflects
    /// the camera the client is about to report.
    fn mirror_aim_to_state(&self) {
        if let Some(state) = self.bot_state.as_ref() {
            state.borrow_mut().aim = *self.aim.aim();
        }
    }

    /// Act on `updateSync()` / `reconnect(ms)` flags scripts set on the bot state.
    async fn poll_bot_actions(&mut self) {
        let (force_sync, reconnect_in, spawn_requested) = match &self.bot_state {
            Some(state) => {
                let mut s = state.borrow_mut();
                (
                    std::mem::take(&mut s.force_sync),
                    s.reconnect_in_ms.take(),
                    std::mem::take(&mut s.spawn_requested),
                )
            }
            None => return,
        };
        // `sampSpawnPlayer()` — explicit spawn request (RakSAMP Lite's "reqspawn"). Usually NOT needed
        // (the server drives the spawn via RequestSpawnResponse(allow==2) after login); this is the
        // manual override. Sets `spawn_requested` so a non-2 allow still spawns us.
        if spawn_requested && self.spectating {
            self.spawn_requested = true;
            let payload = RequestSpawn.encode();
            self.send_rpc(RpcId::RequestSpawn as u8, payload).await;
        }
        if force_sync && matches!(self.state, ConnectionState::Spawned) {
            self.on_sync_tick(true).await;
        }
        if let Some(ms) = reconnect_in {
            self.transition(ConnectionState::Disconnected);
            self.closed = true;
            let _ = self.transport.disconnect().await;
            self.reconnect_timer = Some(Box::pin(time::sleep(Duration::from_millis(ms))));
        }
    }

    async fn step(&mut self) -> Step {
        let closed = self.closed;
        let spawned = matches!(self.state, ConnectionState::Spawned);
        // Arizona pre-spawn spectating also drives the sync tick (to send spectator sync).
        let ticking = spawned || self.spectating;
        let has_reconnect = self.reconnect_timer.is_some();
        let has_sync = self.sync_timer.is_some();
        let has_update = self.update_timer.is_some();
        let Driver {
            transport,
            sync_timer,
            reconnect_timer,
            update_timer,
            ..
        } = self;
        tokio::select! {
            biased;
            _ = poll_sleep(reconnect_timer), if has_reconnect => Step::Reconnect,
            _ = poll_interval(sync_timer), if ticking && has_sync => Step::SyncTick,
            _ = poll_interval(update_timer), if has_update => Step::Update,
            event = transport.recv(), if !closed => Step::Event(event),
        }
    }

    async fn on_rak_event(&mut self, event: RakEvent) {
        match event {
            RakEvent::Connected { body } => self.on_connected(&body).await,
            RakEvent::Rpc { id, payload } => self.on_rpc(id, &payload).await,
            RakEvent::Packet { data } => self.on_packet(&data),
            RakEvent::Disconnected(reason) => self.on_disconnect(reason),
        }
    }

    async fn on_connected(&mut self, body: &[u8]) {
        let (player_id, cookie) = match samp_proto::parse_connect(body) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(%error, "failed to parse connect body");
                self.on_disconnect(DisconnectReason::Rejected);
                return;
            }
        };
        self.local_id = player_id;
        let challenge_response = cookie.0 ^ CHALLENGE_XOR;
        let gpci = self
            .config
            .gpci
            .clone()
            .unwrap_or_else(samp_proto::generate_gpci);
        let payload = {
            let join = ClientJoin {
                version: SAMP_VERSION_0_3_7,
                modded: false,
                nick: self.config.nick.as_str(),
                challenge_response,
                auth: gpci.as_str(),
                client_version: self.config.client_version.as_str(),
                // Append the trailing `challengeResponse2` whenever a script is attached, so a Lua
                // `onSendClientJoin` rewrite (e.g. the Arizona variant) has the 7th field to keep.
                // Vanilla servers ignore the trailing bytes, so this is safe.
                duplicate_challenge_response: self.bot_state.is_some(),
            };
            join.encode()
        };
        self.transition(ConnectionState::RakNetConnected);
        self.pending.push_back(ClientEvent::Connected);
        // The join goes through the `onSendRPC` chokepoint, so a script can rewrite it before it
        // hits the wire. Scripts send their own post-join packets (e.g. Arizona CEF) on `onConnect`;
        // the outbox is flushed after this step.
        self.send_rpc(RpcId::ClientJoin as u8, payload).await;
        self.registry.dispatch_lifecycle("onConnect");
        self.transition(ConnectionState::Joining);
    }

    async fn on_rpc(&mut self, id: u8, payload: &[u8]) {
        tracing::debug!(rpc_id = id, len = payload.len(), state = ?self.state, "inbound RPC");
        if let Some(decoded) = decode_inbound_rpc(id, payload) {
            tracing::debug!(target: "samp_client::driver", "  └─ {decoded}");
        }
        // Registered handlers (scripts/observers) see the RPC first: a Drop consumes it before the
        // FSM, a Rewrite swaps the body the FSM then processes.
        let rewritten;
        let payload = match self.registry.dispatch_rpc(Direction::Incoming, id, payload) {
            Verdict::Pass => payload,
            Verdict::Drop => return,
            Verdict::Rewrite(bytes) => {
                rewritten = bytes;
                rewritten.as_slice()
            }
        };
        self.track_state(id, payload);
        self.aim_follow(id, payload);
        self.emulation_incoming(id, payload).await;
        match (id, &self.state) {
            (RPC_INIT_GAME, ConnectionState::Joining) => self.on_init_game(payload).await,
            (RPC_REQUEST_CLASS, ConnectionState::ClassSelection) => {
                self.on_class_response(payload).await
            }
            // The server drives the spawn with `RequestSpawnResponse` while we're spectating
            // (ClassSelection/ClassSelected), or it answers an explicit `sampSpawnPlayer()` RequestSpawn.
            (
                RPC_REQUEST_SPAWN,
                ConnectionState::ClassSelection | ConnectionState::ClassSelected,
            ) => self.on_spawn_response(payload).await,
            (RPC_TOGGLE_SPECTATING, _) => self.on_toggle_spectating(payload).await,
            (RPC_CONNECTION_REJECTED, _) => self.on_disconnect(DisconnectReason::Rejected),
            (RPC_CLIENT_MESSAGE, _) => self.on_client_message(payload),
            (RPC_CHAT, _) => self.on_player_chat(payload),
            _ => {}
        }
    }

    /// Update the shared local-player state from money/interior/vehicle/camera RPCs so `getBotMoney`
    /// etc. return live values. Decodes via the typed registry; unknown ids are ignored.
    fn track_state(&mut self, id: u8, payload: &[u8]) {
        use crate::state::InVehicleData;
        use samp_proto::events::FieldValue as F;
        let Some(state) = &self.bot_state else {
            return;
        };
        let values = match samp_proto::events::decode_incoming(id, payload) {
            Some(Ok((_, values))) => values,
            _ => return,
        };
        let mut s = state.borrow_mut();
        match id {
            18 => {
                if let Some(F::I32(money)) = values.first() {
                    s.money += money; // GivePlayerMoney is additive
                }
            }
            20 => s.money = 0, // ResetPlayerMoney
            156 => {
                if let Some(F::U8(interior)) = values.first() {
                    s.interior = *interior; // SetInterior
                }
            }
            70 => {
                if let Some(F::U16(vehicle)) = values.first() {
                    // PutPlayerInVehicle
                    s.vehicle = Some(InVehicleData {
                        id: *vehicle,
                        ..InVehicleData::default()
                    });
                }
            }
            71 => s.vehicle = None, // RemovePlayerFromVehicle
            157 => {
                if let Some(F::Vec3(pos)) = values.first() {
                    s.camera_pos = *pos; // SetCameraPosition
                }
            }
            _ => {}
        }
    }

    /// Follow server repositions for aim-sync: `SetPlayerPos` while spawned, and the camera RPCs
    /// before spawn, move the bot and regenerate the aim (ported from `aim_fix_updated.lua`).
    fn aim_follow(&mut self, id: u8, payload: &[u8]) {
        let spawned = matches!(self.state, ConnectionState::Spawned);
        let new_pos = match id {
            12 | 13 if spawned => decode_first_vec3(id, payload),
            157 if !spawned => decode_first_vec3(id, payload),
            82 if !spawned => decode_interpolate_dest(payload),
            _ => return,
        };
        let Some(new_pos) = new_pos else {
            return;
        };
        self.sync.position = new_pos;
        self.mirror_to_state();
        let in_vehicle = self.in_vehicle();
        self.aim
            .on_reposition(new_pos, self.sync.quaternion, in_vehicle);
    }

    /// Run standard-client emulation over an incoming RPC and send anything it produces.
    async fn emulation_incoming(&mut self, id: u8, payload: &[u8]) {
        let Some(state) = self.bot_state.as_ref() else {
            return;
        };
        let msgs = self
            .emulation
            .on_incoming_rpc(&mut state.borrow_mut(), id, payload);
        self.send_outbound(msgs).await;
    }

    /// Send emulation-produced packets/RPCs. These are the client's own, so they bypass the script
    /// chokepoints.
    async fn send_outbound(&mut self, msgs: Vec<OutboundMsg>) {
        for msg in msgs {
            let result = match msg {
                OutboundMsg::Packet {
                    data,
                    reliability,
                    channel,
                } => {
                    self.transport
                        .send(data, reliability_from_wire(reliability), channel)
                        .await
                }
                OutboundMsg::Rpc { id, payload } => self.transport.rpc(id, payload).await,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "failed to send emulation message");
                self.on_disconnect(DisconnectReason::ConnectionLost);
                return;
            }
        }
    }

    /// An incoming raw packet (`data[0]` = id). The client does not process incoming packets itself;
    /// the `onReceivePacket` chokepoint lets scripts observe (or, for the game, drop) them. With
    /// emulation on, a `VehicleSync` that targets the bot's own vehicle is treated as a hijack: the
    /// bot drops the vehicle and the packet.
    fn on_packet(&mut self, data: &[u8]) {
        let Some(&id) = data.first() else {
            return;
        };
        tracing::debug!(packet_id = id, len = data.len(), "inbound packet");
        if id == 220 {
            tracing::debug!(
                sub_id = data.get(1).copied().unwrap_or(0),
                payload = %data.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "inbound 220",
            );
        }
        if id == SyncPacketId::VehicleSync as u8 {
            let hijack = self
                .bot_state
                .as_ref()
                .is_some_and(|state| self.emulation.is_vehicle_hijack(&state.borrow(), data));
            if hijack {
                if let Some(state) = self.bot_state.as_ref() {
                    state.borrow_mut().vehicle = None;
                }
                tracing::info!("emulation: refused a vehicle-sync hijack of our vehicle");
                return;
            }
        }
        let _ = self.registry.dispatch_packet(Direction::Incoming, id, data);
    }

    fn on_client_message(&mut self, payload: &[u8]) {
        match ServerMessage::decode(payload) {
            Ok(msg) => self.pending.push_back(ClientEvent::ServerMessage {
                color: msg.color,
                text: samp_proto::decode_cp1251(&msg.text),
            }),
            Err(error) => tracing::trace!(%error, "failed to decode client message"),
        }
    }

    fn on_player_chat(&mut self, payload: &[u8]) {
        match ChatMessage::decode(payload) {
            Ok(msg) => self.pending.push_back(ClientEvent::Chat {
                player_id: msg.player_id,
                text: samp_proto::decode_cp1251(&msg.text),
            }),
            Err(error) => tracing::trace!(%error, "failed to decode player chat"),
        }
    }

    async fn on_init_game(&mut self, payload: &[u8]) {
        let init = match InitGame::decode(payload) {
            Ok(init) => init,
            Err(error) => {
                tracing::warn!(%error, "failed to decode init game");
                return;
            }
        };
        self.local_id = init.local_player_id;
        let host_name = init.host_name;
        self.transition(ConnectionState::Joined {
            local_id: self.local_id,
            host_name: host_name.clone(),
        });
        self.pending.push_back(ClientEvent::Joined {
            local_id: self.local_id,
            host_name,
        });
        // A script's post-join packets (e.g. a server's validation sequence) go out on `onInitGame`
        // or on its own Lua `wait()`-timed task; the normal class → spawn flow proceeds immediately.
        // Servers that need validation only require it within a window, not strictly before spawn.
        self.registry.dispatch_lifecycle("onInitGame");
        self.flush_outbox().await;
        self.request_spawn_class().await;
    }

    /// After join, request a class and spectate. Like RakSAMP Lite (`RPC_InitGame → RequestClass`) we
    /// do NOT request spawn here — the server drives the spawn via `RequestSpawnResponse(allow==2)`
    /// (or the script calls `sampSpawnPlayer()`). Until then we send spectator sync.
    async fn request_spawn_class(&mut self) {
        let payload = RequestClass {
            class: self.config.default_class,
        }
        .encode();
        self.send_rpc(RpcId::RequestClass as u8, payload).await;
        self.spectating = true;
        self.transition(ConnectionState::ClassSelection);
    }

    async fn on_class_response(&mut self, payload: &[u8]) {
        let response = match RequestClassResponse::decode(payload) {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "failed to decode class response");
                return;
            }
        };
        if response.allowed {
            self.sync.position = response.spawn_position;
            self.mirror_to_state();
        }
        // Like RakSAMP Lite, do NOT auto-request spawn — keep spectating and let the server drive the
        // spawn via `RequestSpawnResponse(allow==2)` (it sends that after login on Arizona). Requesting
        // spawn here would prompt an early allow and spawn us before login → "ОШИБКА 7721".
        self.transition(ConnectionState::ClassSelected);
    }

    async fn on_spawn_response(&mut self, payload: &[u8]) {
        let response = match RequestSpawnResponse::decode(payload) {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "failed to decode spawn response");
                return;
            }
        };
        // RakSAMP Lite's RPC_RequestSpawnResponse condition (0x45ace0): spawn when `allow == 2`, or
        // when `allow != 0` while we explicitly requested spawn. On Arizona the server sends
        // `allow == 2` after login — so this is the server-driven spawn, no explicit client call.
        let allow = response.allow;
        if allow != 2 && !(allow != 0 && self.spawn_requested) {
            tracing::debug!(allow, "server has not authorised spawn yet");
            return;
        }
        self.spectating = false;
        self.spawn_requested = false;
        self.enter_spawned().await;
    }

    /// `TogglePlayerSpectating` (RPC 124): the Arizona server toggles spectate ON during login and
    /// OFF afterwards. A `1 → 0` transition means "drop out of spectate" → spawn, exactly like
    /// RakSAMP Lite's `sub_45B260 → Net_Spawn`. This is the server-driven spawn, no explicit call.
    async fn on_toggle_spectating(&mut self, payload: &[u8]) {
        let toggle = payload.iter().take(4).any(|&b| b != 0);
        let was_spectating = self.server_spectating;
        self.server_spectating = toggle;
        if was_spectating && !toggle && !matches!(self.state, ConnectionState::Spawned) {
            self.spectating = false;
            self.enter_spawned().await;
        }
    }

    /// Send `RPC_Spawn` and enter the `Spawned` state — the single convergence point for every spawn
    /// trigger. We spectate (spectator-sync 212) after join until one of these fires:
    /// - PRIMARY: the login `DialogResponse` (RPC 62) sent while spectating sets `spawn_after_login`,
    ///   and the `next_event` pump then calls here (the order the real Arizona client uses).
    /// - FALLBACK: `TogglePlayerSpectating` (RPC 124) on a `1 → 0` transition (`on_toggle_spectating`),
    ///   or `RequestSpawnResponse(allow==2)` (`on_spawn_response`) — these cover non-Arizona servers.
    /// - MANUAL: `sampSpawnPlayer()` sends `RequestSpawn`, whose response then spawns us.
    async fn enter_spawned(&mut self) {
        let payload = Spawn.encode();
        self.send_rpc(RpcId::Spawn as u8, payload).await;
        self.sync.health = SPAWN_HEALTH;
        self.sync.quaternion = Quaternion {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        };
        self.transition(ConnectionState::Spawned);
        self.mirror_to_state();
        let in_vehicle = self.in_vehicle();
        self.aim.arm(
            Instant::now(),
            self.sync.position,
            self.sync.quaternion,
            in_vehicle,
        );
        self.pending.push_back(ClientEvent::Spawned);
    }

    /// Arizona connect flow (RakSAMP Lite model): after join send the entity-streamer init RPC and
    /// `RequestClass`, then spectate (spectator sync via the sync tick). We do NOT send `RequestSpawn`
    /// — the server drives the spawn by sending `RequestSpawnResponse(allow==2)` after login, exactly
    /// like RakSAMP Lite's `RPC_InitGame → RequestClass … RPC_RequestSpawnResponse → Net_Spawn`.
    /// Spawning before login earns "ОШИБКА 7721", so the spawn must be server-driven (or via an
    /// Send a spectator-sync packet (212) — the pre-spawn keepalive while spectating.
    async fn send_spectator_sync(&mut self) {
        let packet = SpectatorSync {
            position: self.sync.position,
        }
        .to_packet();
        if let Err(error) = self
            .transport
            .send(packet, Reliability::UnreliableSequenced, 0)
            .await
        {
            tracing::warn!(%error, "failed to send spectator sync");
            self.on_disconnect(DisconnectReason::ConnectionLost);
        }
    }

    /// Whether the bot is currently in a vehicle (from the shared local-player state).
    fn in_vehicle(&self) -> bool {
        self.bot_state
            .as_ref()
            .is_some_and(|s| s.borrow().in_vehicle())
    }

    /// One sync cycle. The on-foot packet is sent adaptively — on any state change, on a forced
    /// sync (`updateSync()`), or otherwise only at the [`IDLE_SYNC_INTERVAL`] keepalive cadence — so
    /// a stationary bot stops flooding identical packets. Aim and weapon-inventory sends keep their
    /// own cadences and run every cycle.
    async fn on_sync_tick(&mut self, force: bool) {
        // Arizona pre-spawn: spectate (keepalive) while awaiting the login dialog, not on-foot sync.
        if self.spectating && !matches!(self.state, ConnectionState::Spawned) {
            self.send_spectator_sync().await;
            return;
        }
        self.mirror_from_state();
        // Emulation: report the held weapon and the occasional score-ping key blip.
        if let Some(state) = self.bot_state.as_ref() {
            let (weapon, keys) =
                self.emulation
                    .adjust_on_foot(&state.borrow(), self.sync.keys, Instant::now());
            self.sync.weapon = WeaponId(weapon);
            self.sync.keys = keys;
        }
        let now = Instant::now();
        let due = force
            || self.sync != self.last_sync
            || self
                .last_sync_at
                .is_none_or(|at| now.duration_since(at) >= IDLE_SYNC_INTERVAL);
        if due {
            let packet = self.sync.to_packet();
            // The sync packet passes through `onSendPacket` so scripts (e.g. aim-fix) can edit/drop it.
            let to_send = match self.registry.dispatch_packet(
                Direction::Outgoing,
                SyncPacketId::PlayerSync as u8,
                &packet,
            ) {
                Verdict::Drop => None,
                Verdict::Pass => Some(packet),
                Verdict::Rewrite(bytes) => Some(bytes),
            };
            if let Some(packet) = to_send {
                if let Err(error) = self
                    .transport
                    .send(packet, Reliability::UnreliableSequenced, 0)
                    .await
                {
                    tracing::warn!(%error, "failed to send on-foot sync");
                    self.on_disconnect(DisconnectReason::ConnectionLost);
                    return;
                }
                self.last_sync = self.sync;
                self.last_sync_at = Some(now);
            }
        }
        // Emulation: stream the weapon inventory periodically.
        let weapons_msg = if let Some(state) = self.bot_state.as_ref() {
            self.emulation
                .due_weapons_update(&state.borrow(), Instant::now())
        } else {
            None
        };
        if let Some(msg) = weapons_msg {
            self.send_outbound(vec![msg]).await;
        }
        // Native aim-sync: note position (to detect movement) and send a believable aim when due.
        self.aim.on_position(self.sync.position);
        let aim_packet = if self.aim.due(Instant::now()) {
            if let Some(state) = self.bot_state.as_ref() {
                self.emulation
                    .spoof_aim(self.aim.aim_mut(), &state.borrow());
            }
            Some(self.aim.encode())
        } else {
            None
        };
        self.mirror_aim_to_state();
        if let Some(aim_packet) = aim_packet {
            if let Err(error) = self
                .transport
                .send(aim_packet, Reliability::UnreliableSequenced, 0)
                .await
            {
                tracing::warn!(%error, "failed to send aim sync");
                self.on_disconnect(DisconnectReason::ConnectionLost);
            }
        }
    }

    async fn on_reconnect(&mut self) {
        self.reconnect_timer = None;
        self.closed = false;
        match self.transport.reconnect().await {
            Ok(()) => {
                self.local_id = PlayerId::default();
                self.sync = OnFootSync::default();
                self.last_sync = OnFootSync::default();
                self.last_sync_at = None;
                self.transition(ConnectionState::Connecting);
            }
            Err(error) => {
                tracing::warn!(%error, "reconnect attempt failed");
                self.on_disconnect(DisconnectReason::ConnectionLost);
            }
        }
    }

    fn on_transport_closed(&mut self) {
        self.closed = true;
        if self.reconnect_timer.is_none() && self.state != ConnectionState::Disconnected {
            self.on_disconnect(DisconnectReason::ConnectionLost);
        }
    }

    fn on_disconnect(&mut self, reason: DisconnectReason) {
        self.aim.reset();
        if let Some(state) = self.bot_state.as_ref() {
            self.emulation.reset(&mut state.borrow_mut());
        }
        self.transition(ConnectionState::Disconnected);
        self.pending
            .push_back(ClientEvent::Disconnected(describe_reason(reason)));
        if reason != DisconnectReason::Local {
            self.schedule_reconnect();
        }
    }

    async fn send_rpc(&mut self, rpc_id: u8, payload: Vec<u8>) {
        // Registered handlers see outgoing RPCs too (the `onSend*` events): Drop cancels the send,
        // Rewrite changes the body that goes on the wire.
        let payload = match self
            .registry
            .dispatch_rpc(Direction::Outgoing, rpc_id, &payload)
        {
            Verdict::Pass => payload,
            Verdict::Drop => return,
            Verdict::Rewrite(bytes) => bytes,
        };
        if let Err(error) = self.transport.rpc(rpc_id, payload).await {
            tracing::warn!(rpc_id, %error, "failed to send rpc");
            self.on_disconnect(DisconnectReason::ConnectionLost);
        }
        // The login dialog was just answered while spectating → spawn next, the order the real client
        // uses (deferred to the pump loop to avoid recursing through `send_rpc`).
        if !self.closed
            && self.spectating
            && rpc_id == RpcId::DialogResponse as u8
            && !matches!(self.state, ConnectionState::Spawned)
        {
            self.spawn_after_login = true;
        }
    }

    /// Send a chat line as the local player (`RPC_Chat`, client→server `[u8 len][text]`). `text` is
    /// raw bytes in the server's encoding (cp1251 for Arizona); callers transcode before calling.
    pub(crate) async fn send_chat(&mut self, text: &[u8]) {
        let payload = ChatOutgoing { text }.encode();
        self.send_rpc(RpcId::Chat as u8, payload).await;
    }

    fn transition(&mut self, next: ConnectionState) {
        self.state = next.clone();
        // Arm the sync tick while Spawned (on-foot sync) OR during the Arizona pre-spawn spectate
        // phase (spectator sync keepalive while waiting for the server to drive the spawn).
        if matches!(self.state, ConnectionState::Spawned) || self.spectating {
            if self.sync_timer.is_none() {
                let period = self.config.sync_interval.max(Duration::from_millis(1));
                let mut interval = time::interval(period);
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                self.sync_timer = Some(interval);
            }
        } else {
            self.sync_timer = None;
        }
        self.pending.push_back(ClientEvent::StateChanged(next));
    }

    fn schedule_reconnect(&mut self) {
        if self.reconnect_timer.is_none() {
            let delay = self.config.reconnect_delay;
            self.reconnect_timer = Some(Box::pin(time::sleep(delay)));
        }
    }
}

async fn poll_sleep(timer: &mut Option<Pin<Box<Sleep>>>) {
    match timer {
        Some(sleep) => sleep.as_mut().await,
        None => future::pending().await,
    }
}

async fn poll_interval(timer: &mut Option<Interval>) {
    match timer {
        Some(interval) => {
            interval.tick().await;
        }
        None => future::pending::<()>().await,
    }
}

/// Decode the first `vector3` field of an incoming RPC (e.g. `SetPlayerPos`/`SetCameraPos`).
fn decode_first_vec3(id: u8, payload: &[u8]) -> Option<Vector3> {
    use samp_proto::events::FieldValue as F;
    match samp_proto::events::decode_incoming(id, payload) {
        Some(Ok((_, values))) => match values.first() {
            Some(F::Vec3(v)) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

/// `InterpolateCamera` (82) destination, when it sets position: `{set_pos bool, from_pos vec3,
/// dest_pos vec3, ...}` — return `dest_pos` only when `set_pos` is true.
fn decode_interpolate_dest(payload: &[u8]) -> Option<Vector3> {
    use samp_proto::events::FieldValue as F;
    match samp_proto::events::decode_incoming(82, payload) {
        Some(Ok((_, values))) if matches!(values.first(), Some(F::Bool(true))) => {
            match values.get(2) {
                Some(F::Vec3(v)) => Some(*v),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Decode an inbound RPC body into a human-readable struct for logging — turns the raw bytes into
/// named fields (cp1251 text rendered as strings). Covers the dialog/chat specials plus everything in
/// the typed incoming registry; returns `None` for ids with no decoder.
fn decode_inbound_rpc(id: u8, payload: &[u8]) -> Option<String> {
    use samp_proto::events::FieldValue as F;
    match id {
        61 => samp_proto::ShowDialog::decode(payload).ok().map(|d| {
            format!(
                "ShowDialog {{ dialog_id: {}, style: {}, title: {:?}, button1: {:?}, button2: {:?} }}",
                d.dialog_id,
                d.style,
                samp_proto::decode_cp1251(&d.title),
                samp_proto::decode_cp1251(&d.button1),
                samp_proto::decode_cp1251(&d.button2),
            )
        }),
        93 => samp_proto::ServerMessage::decode(payload).ok().map(|m| {
            format!(
                "ClientMessage {{ color: {:08X}, text: {:?} }}",
                m.color,
                samp_proto::decode_cp1251(&m.text)
            )
        }),
        101 => samp_proto::ChatMessage::decode(payload).ok().map(|m| {
            format!(
                "Chat {{ player_id: {}, text: {:?} }}",
                m.player_id.0,
                samp_proto::decode_cp1251(&m.text)
            )
        }),
        _ => {
            let (name, fields) = samp_proto::events::decode_incoming(id, payload)?.ok()?;
            let body = fields
                .iter()
                .map(|f| match f {
                    F::Bytes(b) => format!("{:?}", samp_proto::decode_cp1251(b)),
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("{name} {{ {body} }}"))
        }
    }
}

/// Map a RakNet wire reliability value (`0..=4`, as carried in [`OutboundMsg::Packet`]) to the
/// transport's [`Reliability`]. Unknown values fall back to reliable-ordered.
fn reliability_from_wire(value: u8) -> Reliability {
    match value {
        0 => Reliability::Unreliable,
        1 => Reliability::UnreliableSequenced,
        2 => Reliability::Reliable,
        4 => Reliability::ReliableSequenced,
        _ => Reliability::ReliableOrdered,
    }
}

fn describe_reason(reason: DisconnectReason) -> String {
    let text = match reason {
        DisconnectReason::AttemptFailed => "connection attempt failed",
        DisconnectReason::ServerFull => "server full",
        DisconnectReason::Banned => "banned",
        DisconnectReason::InvalidPassword => "invalid password",
        DisconnectReason::ClosedByServer => "closed by server",
        DisconnectReason::ConnectionLost => "connection lost",
        DisconnectReason::Rejected => "connection rejected",
        DisconnectReason::Timeout => "connection timed out",
        DisconnectReason::Local => "disconnected locally",
    };
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct TransportLog {
        rpcs: Vec<(u8, Vec<u8>)>,
        packets: Vec<Vec<u8>>,
        disconnects: usize,
        reconnects: usize,
    }

    impl TransportLog {
        fn rpc_ids(&self) -> Vec<u8> {
            self.rpcs.iter().map(|(id, _)| *id).collect()
        }

        fn sync_count(&self) -> usize {
            self.packets
                .iter()
                .filter(|packet| packet.first() == Some(&(SyncPacketId::PlayerSync as u8)))
                .count()
        }
    }

    struct ScriptedTransport {
        script: VecDeque<RakEvent>,
        pend_when_empty: bool,
        log: Arc<Mutex<TransportLog>>,
    }

    impl ScriptedTransport {
        fn new(script: Vec<RakEvent>, pend_when_empty: bool) -> (Self, Arc<Mutex<TransportLog>>) {
            let log = Arc::new(Mutex::new(TransportLog::default()));
            let transport = Self {
                script: script.into(),
                pend_when_empty,
                log: log.clone(),
            };
            (transport, log)
        }
    }

    impl Transport for ScriptedTransport {
        async fn send(
            &self,
            data: Vec<u8>,
            _reliability: Reliability,
            _channel: u8,
        ) -> raknet::Result<()> {
            self.log.lock().expect("log poisoned").packets.push(data);
            Ok(())
        }

        async fn rpc(&self, rpc_id: u8, payload: Vec<u8>) -> raknet::Result<()> {
            self.log
                .lock()
                .expect("log poisoned")
                .rpcs
                .push((rpc_id, payload));
            Ok(())
        }

        async fn disconnect(&self) -> raknet::Result<()> {
            self.log.lock().expect("log poisoned").disconnects += 1;
            Ok(())
        }

        async fn recv(&mut self) -> Option<RakEvent> {
            if let Some(event) = self.script.pop_front() {
                Some(event)
            } else if self.pend_when_empty {
                future::pending().await
            } else {
                None
            }
        }

        async fn reconnect(&mut self) -> raknet::Result<()> {
            self.log.lock().expect("log poisoned").reconnects += 1;
            Ok(())
        }
    }

    fn test_config() -> ClientConfig {
        ClientConfig::builder(
            "127.0.0.1:7777".parse::<SocketAddr>().expect("addr"),
            "Tester",
        )
        .sync_interval(Duration::from_millis(100))
        .reconnect_delay(Duration::from_secs(5))
        .build()
    }

    /// A `CONNECTION_REQUEST_ACCEPTED` body the real [`samp_proto::parse_connect`] accepts (12 bytes:
    /// `[u32 ip][u16 port][u16 playerId][u32 cookie]`). The values are irrelevant to the assertions —
    /// the player id is overwritten by `InitGame` and the cookie only feeds the (unsent-here) join.
    fn connect_body() -> Vec<u8> {
        vec![0u8; 12]
    }

    fn happy_script() -> Vec<RakEvent> {
        vec![
            RakEvent::Connected {
                body: connect_body(),
            },
            RakEvent::Rpc {
                id: RPC_INIT_GAME,
                payload: InitGame {
                    local_player_id: PlayerId(42),
                    host_name: "Test Server".to_string(),
                }
                .encode(),
            },
            RakEvent::Rpc {
                id: RPC_REQUEST_CLASS,
                payload: RequestClassResponse {
                    allowed: true,
                    spawn_position: Vector3 {
                        x: 1.0,
                        y: 2.0,
                        z: 3.0,
                    },
                    ..RequestClassResponse::default()
                }
                .encode(),
            },
            RakEvent::Rpc {
                id: RPC_REQUEST_SPAWN,
                payload: RequestSpawnResponse { allow: 2 }.encode(),
            },
        ]
    }

    #[tokio::test]
    async fn reaches_spawned_emitting_events_in_order() {
        let (transport, log) = ScriptedTransport::new(happy_script(), false);
        let mut driver = Driver::new(test_config(), transport);

        let mut milestones = Vec::new();
        while let Some(event) = driver.next_event().await {
            match event {
                ClientEvent::Connected => milestones.push("connected"),
                ClientEvent::Joined {
                    local_id,
                    host_name,
                } => {
                    assert_eq!(local_id, PlayerId(42));
                    assert_eq!(host_name, "Test Server");
                    milestones.push("joined");
                }
                ClientEvent::Spawned => {
                    milestones.push("spawned");
                    break;
                }
                ClientEvent::Disconnected(reason) => panic!("unexpected disconnect: {reason}"),
                ClientEvent::ServerMessage { .. } | ClientEvent::Chat { .. } => {}
                ClientEvent::StateChanged(_) => {}
            }
        }

        assert_eq!(milestones, ["connected", "joined", "spawned"]);
        assert_eq!(driver.state(), &ConnectionState::Spawned);

        let log = log.lock().expect("log poisoned");
        // Server-driven spawn (RakSAMP Lite model): we send ClientJoin + RequestClass, then the
        // server's RequestSpawnResponse(allow==2) makes us send Spawn. We never send RequestSpawn.
        assert_eq!(
            log.rpc_ids(),
            vec![
                RpcId::ClientJoin as u8,
                RpcId::RequestClass as u8,
                RpcId::Spawn as u8,
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_schedules_reconnect() {
        let (transport, log) = ScriptedTransport::new(
            vec![RakEvent::Disconnected(DisconnectReason::InvalidPassword)],
            false,
        );
        let mut driver = Driver::new(test_config(), transport);

        let mut disconnect_message = None;
        while let Some(event) = driver.next_event().await {
            if let ClientEvent::Disconnected(message) = event {
                disconnect_message = Some(message);
                break;
            }
        }

        assert_eq!(disconnect_message.as_deref(), Some("invalid password"));
        assert!(driver.reconnect_scheduled());
        assert_eq!(driver.state(), &ConnectionState::Disconnected);

        tokio::time::advance(test_config().reconnect_delay).await;
        let event = driver.next_event().await;
        assert!(matches!(
            event,
            Some(ClientEvent::StateChanged(ConnectionState::Connecting))
        ));
        assert_eq!(driver.state(), &ConnectionState::Connecting);
        assert!(!driver.reconnect_scheduled());
        assert_eq!(log.lock().expect("log poisoned").reconnects, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn connection_lost_without_event_still_reconnects() {
        let (transport, _log) = ScriptedTransport::new(Vec::new(), false);
        let mut driver = Driver::new(test_config(), transport);

        // Drain the initial `Connecting` state event, then the transport closes silently.
        let mut saw_disconnect = false;
        while let Some(event) = driver.next_event().await {
            if let ClientEvent::Disconnected(message) = event {
                assert_eq!(message, "connection lost");
                saw_disconnect = true;
                break;
            }
        }

        assert!(saw_disconnect);
        assert!(driver.reconnect_scheduled());
    }

    #[test]
    fn track_state_updates_money_and_vehicle() {
        let (transport, _log) = ScriptedTransport::new(Vec::new(), false);
        let state =
            crate::state::LocalPlayer::shared("Bot".to_string(), "127.0.0.1:7777".parse().unwrap());
        let mut driver = Driver::new(test_config(), transport).with_bot_state(state.clone());

        // GivePlayerMoney (18) is additive; ResetPlayerMoney (20) zeroes.
        driver.track_state(18, &500i32.to_le_bytes());
        driver.track_state(18, &250i32.to_le_bytes());
        assert_eq!(state.borrow().money, 750);
        driver.track_state(20, &[]);
        assert_eq!(state.borrow().money, 0);

        // PutPlayerInVehicle (70): vehicleId u16 then seat u8.
        driver.track_state(70, &[0x2A, 0x00, 0x00]);
        assert_eq!(state.borrow().vehicle_id(), 42);
        driver.track_state(71, &[]); // RemovePlayerFromVehicle
        assert!(state.borrow().vehicle.is_none());
    }

    #[tokio::test]
    async fn on_foot_sync_is_adaptive() {
        let (transport, log) = ScriptedTransport::new(Vec::new(), false);
        let state =
            crate::state::LocalPlayer::shared("Bot".to_string(), "127.0.0.1:7777".parse().unwrap());
        let mut driver = Driver::new(test_config(), transport).with_bot_state(state.clone());

        let sync_count = || log.lock().expect("log poisoned").sync_count();

        // First cycle always sends (nothing sent yet).
        driver.on_sync_tick(false).await;
        assert_eq!(sync_count(), 1);
        // Unchanged within the idle window → no resend.
        driver.on_sync_tick(false).await;
        assert_eq!(sync_count(), 1, "identical state should not resend");
        // A state change resends immediately.
        state.borrow_mut().on_foot.position.x = 5.0;
        driver.on_sync_tick(false).await;
        assert_eq!(sync_count(), 2, "a change should resend");
        // A forced sync always sends, even unchanged.
        driver.on_sync_tick(true).await;
        assert_eq!(sync_count(), 3, "force should always send");
    }

    #[tokio::test(start_paused = true)]
    async fn sync_loop_sends_while_spawned() {
        let (transport, log) = ScriptedTransport::new(happy_script(), true);
        let mut driver = Driver::new(test_config(), transport);

        loop {
            match driver.next_event().await {
                Some(ClientEvent::Spawned) => break,
                Some(_) => continue,
                None => panic!("transport closed before spawn"),
            }
        }
        assert_eq!(driver.state(), &ConnectionState::Spawned);

        // Drive the FSM directly rather than from a background task — a registry-bearing driver is
        // `!Send` and cannot be `tokio::spawn`ed. With the clock paused, each `step()` awaits the
        // sync interval, which auto-advances and yields a `SyncTick`.
        let mut sync_count = 0;
        for _ in 0..16 {
            match driver.step().await {
                Step::SyncTick => driver.on_sync_tick(false).await,
                Step::Event(Some(event)) => driver.on_rak_event(event).await,
                Step::Update => driver.registry.tick(),
                _ => {}
            }
            sync_count = log.lock().expect("log poisoned").sync_count();
            if sync_count >= 1 {
                break;
            }
        }

        assert!(sync_count >= 1, "expected at least one on-foot sync packet");
        let packet = log
            .lock()
            .expect("log poisoned")
            .packets
            .iter()
            .find(|packet| packet.first() == Some(&(SyncPacketId::PlayerSync as u8)))
            .cloned()
            .expect("sync packet recorded");
        // The PlayerSync id byte followed by the real 68-byte on-foot sync body.
        assert_eq!(packet.first(), Some(&(SyncPacketId::PlayerSync as u8)));
        assert_eq!(packet.len(), samp_proto::ON_FOOT_SYNC_LEN + 1);
    }
}
