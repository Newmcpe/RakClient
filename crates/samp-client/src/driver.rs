//! The connection state machine.
//!
//! [`Driver`] is generic over a [`Transport`] and a [`Codec`] so it can be unit-tested against a
//! scripted fake while production instantiates it with the real RakNet/`samp_proto` implementations.
//! It is pumped one event at a time through [`Driver::next_event`], which internally `select!`s over
//! incoming transport events, the on-foot sync interval (while `Spawned`), and the reconnect timer.

use std::collections::VecDeque;
use std::future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use raknet::{DisconnectReason, RakEvent, Reliability};
use samp_proto::{
    ClientJoin, Direction, OnFootSync, OutboundMsg, PlayerId, Quaternion, RpcId, SyncPacketId,
    Vector3, Verdict, WeaponId, CHALLENGE_XOR, SAMP_VERSION_0_3_7,
};
use tokio::time::{self, Interval, MissedTickBehavior, Sleep};

use crate::aim::AimSync;
use crate::codec::Codec;
use crate::registry::PacketRegistry;
use crate::state::SharedBotState;
use crate::transport::Transport;
use crate::{ClientConfig, ClientEvent, ConnectionState};

/// How often the registry's `on_update` handlers fire while connected (drives script timers).
const UPDATE_INTERVAL: Duration = Duration::from_millis(100);

const RPC_INIT_GAME: u8 = RpcId::InitGame as u8;
const RPC_REQUEST_CLASS: u8 = RpcId::RequestClass as u8;
const RPC_REQUEST_SPAWN: u8 = RpcId::RequestSpawn as u8;
const RPC_CONNECTION_REJECTED: u8 = RpcId::ConnectionRejected as u8;
const RPC_CLIENT_MESSAGE: u8 = RpcId::ClientMessage as u8;
const RPC_CHAT: u8 = RpcId::Chat as u8;
const RPC_SHOW_DIALOG: u8 = RpcId::ShowDialog as u8;

const SPAWN_HEALTH: u8 = 100;

/// What woke the `select!` in [`Driver::step`].
enum Step {
    Event(Option<RakEvent>),
    SyncTick,
    Reconnect,
    Update,
}

pub(crate) struct Driver<T: Transport, C: Codec> {
    config: ClientConfig,
    transport: T,
    codec: C,
    state: ConnectionState,
    pending: VecDeque<ClientEvent>,
    local_id: PlayerId,
    sync: OnFootSync,
    sync_timer: Option<Interval>,
    reconnect_timer: Option<Pin<Box<Sleep>>>,
    closed: bool,
    /// Packet-handler registry: scripts/observers intercept RPCs here before the FSM sees them.
    registry: PacketRegistry,
    /// Fires the registry's `on_update` handlers; armed only when handlers are registered.
    update_timer: Option<Interval>,
    /// Shared bot state mirrored to/from `sync`, exposing `getBot*`/`setBot*` to scripts.
    bot_state: Option<SharedBotState>,
    /// Native aim-sync emulation; `None` when disabled via config.
    aim: Option<AimSync>,
}

impl<T: Transport, C: Codec> Driver<T, C> {
    pub(crate) fn new(config: ClientConfig, transport: T, codec: C) -> Self {
        let mut pending = VecDeque::new();
        pending.push_back(ClientEvent::StateChanged(ConnectionState::Connecting));
        let aim = config.aim_sync.then(|| {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x1234_5678);
            AimSync::new(seed)
        });
        Self {
            config,
            transport,
            codec,
            state: ConnectionState::Connecting,
            pending,
            local_id: PlayerId::default(),
            sync: OnFootSync::default(),
            sync_timer: None,
            reconnect_timer: None,
            closed: false,
            registry: PacketRegistry::new(),
            update_timer: None,
            bot_state: None,
            aim,
        }
    }

    /// Share a [`SharedBotState`] with the script engine: the driver mirrors `sync` into it and reads
    /// `setBot*` writes back out of it.
    pub(crate) fn with_bot_state(mut self, state: SharedBotState) -> Self {
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
                Step::SyncTick => self.on_sync_tick().await,
                Step::Reconnect => self.on_reconnect().await,
                Step::Update => self.registry.tick(),
            }
            self.flush_outbox().await;
            self.poll_bot_actions().await;
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
                    self.transport
                        .send(data, reliability_from_wire(reliability), channel)
                        .await
                }
                OutboundMsg::Rpc { id, payload } => self.transport.rpc(id, payload).await,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "failed to send script-queued message");
                self.on_disconnect(DisconnectReason::ConnectionLost);
                return;
            }
        }
    }

    /// Push the authoritative `sync` fields into the shared bot state (so `getBot*` reflects what the
    /// server set, e.g. the spawn position).
    fn mirror_to_state(&self) {
        if let Some(state) = &self.bot_state {
            let mut s = state.borrow_mut();
            s.position = self.sync.position;
            s.rotation = self.sync.quaternion;
            s.health = self.sync.health;
            s.armour = self.sync.armour;
            s.weapon = self.sync.weapon.0;
        }
    }

    /// Pull script `setBot*` writes back into `sync` before sending it.
    fn mirror_from_state(&mut self) {
        if let Some(state) = &self.bot_state {
            let s = state.borrow();
            self.sync.position = s.position;
            self.sync.quaternion = s.rotation;
            self.sync.health = s.health;
            self.sync.armour = s.armour;
            self.sync.weapon = WeaponId(s.weapon);
        }
    }

    /// Act on `updateSync()` / `reconnect(ms)` flags scripts set on the bot state.
    async fn poll_bot_actions(&mut self) {
        let (force_sync, reconnect_in) = match &self.bot_state {
            Some(state) => {
                let mut s = state.borrow_mut();
                (std::mem::take(&mut s.force_sync), s.reconnect_in_ms.take())
            }
            None => return,
        };
        if force_sync && matches!(self.state, ConnectionState::Spawned) {
            self.on_sync_tick().await;
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
            _ = poll_interval(sync_timer), if spawned && has_sync => Step::SyncTick,
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
        let (player_id, cookie) = match self.codec.parse_connect(body) {
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
            .unwrap_or_else(|| self.codec.generate_gpci());
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
            self.codec.encode_client_join(&join)
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
        match (id, &self.state) {
            (RPC_INIT_GAME, ConnectionState::Joining) => self.on_init_game(payload).await,
            (RPC_REQUEST_CLASS, ConnectionState::ClassSelection) => {
                self.on_class_response(payload).await
            }
            (RPC_REQUEST_SPAWN, ConnectionState::SpawnRequested) => {
                self.on_spawn_response(payload).await
            }
            (RPC_CONNECTION_REJECTED, _) => self.on_disconnect(DisconnectReason::Rejected),
            (RPC_CLIENT_MESSAGE, _) => self.on_client_message(payload),
            (RPC_CHAT, _) => self.on_player_chat(payload),
            (RPC_SHOW_DIALOG, _) => self.on_show_dialog(payload).await,
            _ => {}
        }
    }

    /// Update the shared bot state from money/interior/vehicle/camera RPCs so `getBotMoney` etc.
    /// return live values. Decodes via the typed registry; unknown ids are ignored.
    fn track_state(&mut self, id: u8, payload: &[u8]) {
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
                    s.vehicle = *vehicle; // PutPlayerInVehicle
                }
            }
            71 => s.vehicle = 0, // RemovePlayerFromVehicle
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
        if self.aim.is_none() {
            return;
        }
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
        if let Some(aim) = self.aim.as_mut() {
            aim.on_reposition(new_pos, self.sync.quaternion, in_vehicle);
        }
    }

    /// An incoming raw packet (`data[0]` = id). The client does not process incoming packets itself;
    /// the `onReceivePacket` chokepoint lets scripts observe (or, for the game, drop) them.
    fn on_packet(&mut self, data: &[u8]) {
        let Some(&id) = data.first() else {
            return;
        };
        let _ = self.registry.dispatch_packet(Direction::Incoming, id, data);
    }

    /// Driver-side auto-login fallback for a server `ShowDialog`. Only fires when an
    /// `account_password` is explicitly configured; otherwise the dialog is left for a Lua
    /// `samp.events.onShowDialog` handler (which answers via `sampSendDialogResponse`). Arizona
    /// gates a valid session behind its login dialog; the password is never logged.
    async fn on_show_dialog(&mut self, payload: &[u8]) {
        let Some(input) = self.config.account_password.clone() else {
            return; // no password set → let a script answer (or no auth needed).
        };
        let dialog = match samp_proto::decode_show_dialog(payload) {
            Ok(dialog) => dialog,
            Err(error) => {
                tracing::warn!(%error, "failed to decode show dialog");
                return;
            }
        };
        tracing::info!(
            dialog_id = dialog.dialog_id,
            style = dialog.style,
            title = %samp_proto::decode_cp1251(&dialog.title),
            "answering server dialog"
        );
        let response =
            samp_proto::encode_dialog_response(dialog.dialog_id, 1, 0xFFFF, input.as_bytes());
        self.send_rpc(RpcId::DialogResponse as u8, response).await;
    }

    fn on_client_message(&mut self, payload: &[u8]) {
        match self.codec.decode_client_message(payload) {
            Ok(msg) => self.pending.push_back(ClientEvent::ServerMessage {
                color: msg.color,
                text: samp_proto::decode_cp1251(&msg.text),
            }),
            Err(error) => tracing::trace!(%error, "failed to decode client message"),
        }
    }

    fn on_player_chat(&mut self, payload: &[u8]) {
        match self.codec.decode_player_chat(payload) {
            Ok(msg) => self.pending.push_back(ClientEvent::Chat {
                player_id: msg.player_id,
                text: samp_proto::decode_cp1251(&msg.text),
            }),
            Err(error) => tracing::trace!(%error, "failed to decode player chat"),
        }
    }

    async fn on_init_game(&mut self, payload: &[u8]) {
        let init = match self.codec.decode_init_game(payload) {
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

    async fn request_spawn_class(&mut self) {
        let payload = self.codec.encode_request_class(self.config.default_class);
        self.send_rpc(RpcId::RequestClass as u8, payload).await;
        self.transition(ConnectionState::ClassSelection);
    }

    async fn on_class_response(&mut self, payload: &[u8]) {
        let response = match self.codec.decode_request_class_response(payload) {
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
        self.transition(ConnectionState::ClassSelected);
        let payload = self.codec.encode_request_spawn();
        self.send_rpc(RpcId::RequestSpawn as u8, payload).await;
        self.transition(ConnectionState::SpawnRequested);
    }

    async fn on_spawn_response(&mut self, payload: &[u8]) {
        let response = match self.codec.decode_request_spawn_response(payload) {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "failed to decode spawn response");
                return;
            }
        };
        // `allow != 0` while a spawn was requested is the server letting us spawn (it is `2` on the
        // normal path). `0` means "not yet" — stay in `SpawnRequested` and wait.
        if response.allow == 0 {
            tracing::debug!("server has not authorised spawn yet");
            return;
        }
        let payload = self.codec.encode_spawn();
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
        if let Some(aim) = self.aim.as_mut() {
            aim.arm(
                Instant::now(),
                self.sync.position,
                self.sync.quaternion,
                in_vehicle,
            );
        }
        self.pending.push_back(ClientEvent::Spawned);
    }

    /// Whether the bot is currently in a vehicle (from the shared bot state).
    fn in_vehicle(&self) -> bool {
        self.bot_state
            .as_ref()
            .is_some_and(|s| s.borrow().vehicle != 0)
    }

    async fn on_sync_tick(&mut self) {
        self.mirror_from_state();
        let body = self.codec.encode_on_foot_sync(&self.sync);
        let mut packet = Vec::with_capacity(body.len() + 1);
        packet.push(SyncPacketId::PlayerSync as u8);
        packet.extend_from_slice(&body);
        // The sync packet passes through `onSendPacket` so scripts (e.g. aim-fix) can edit/drop it.
        let packet = match self.registry.dispatch_packet(
            Direction::Outgoing,
            SyncPacketId::PlayerSync as u8,
            &packet,
        ) {
            Verdict::Drop => return,
            Verdict::Pass => packet,
            Verdict::Rewrite(bytes) => bytes,
        };
        if let Err(error) = self
            .transport
            .send(packet, Reliability::UnreliableSequenced, 0)
            .await
        {
            tracing::warn!(%error, "failed to send on-foot sync");
            self.on_disconnect(DisconnectReason::ConnectionLost);
            return;
        }
        // Native aim-sync: note position (to detect movement) and send a believable aim when due.
        let aim_packet = match self.aim.as_mut() {
            Some(aim) => {
                aim.on_position(self.sync.position);
                aim.due_packet(Instant::now())
            }
            None => None,
        };
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
        if let Some(aim) = self.aim.as_mut() {
            aim.reset();
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
    }

    /// Send a chat line as the local player (`RPC_Chat`, client→server `[u8 len][text]`). `text` is
    /// raw bytes in the server's encoding (cp1251 for Arizona); callers transcode before calling.
    pub(crate) async fn send_chat(&mut self, text: &[u8]) {
        let payload = self.codec.encode_chat(text);
        self.send_rpc(RpcId::Chat as u8, payload).await;
    }

    fn transition(&mut self, next: ConnectionState) {
        self.state = next.clone();
        if matches!(self.state, ConnectionState::Spawned) {
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

    use samp_proto::{InitGame, RequestClassResponse, RequestSpawnResponse, ServerCookie, Vector3};

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

    #[derive(Clone, Default)]
    struct FakeCodec {
        connect: (PlayerId, ServerCookie),
        init: InitGame,
        class_response: RequestClassResponse,
        spawn_response: RequestSpawnResponse,
    }

    impl Codec for FakeCodec {
        fn parse_connect(&self, _body: &[u8]) -> samp_proto::Result<(PlayerId, ServerCookie)> {
            Ok(self.connect)
        }

        fn encode_client_join(&self, _join: &ClientJoin<'_>) -> Vec<u8> {
            Vec::new()
        }

        fn decode_init_game(&self, _payload: &[u8]) -> samp_proto::Result<InitGame> {
            Ok(self.init.clone())
        }

        fn encode_request_class(&self, _class: samp_proto::ClassId) -> Vec<u8> {
            Vec::new()
        }

        fn decode_request_class_response(
            &self,
            _payload: &[u8],
        ) -> samp_proto::Result<RequestClassResponse> {
            Ok(self.class_response.clone())
        }

        fn encode_request_spawn(&self) -> Vec<u8> {
            Vec::new()
        }

        fn decode_request_spawn_response(
            &self,
            _payload: &[u8],
        ) -> samp_proto::Result<RequestSpawnResponse> {
            Ok(self.spawn_response)
        }

        fn encode_spawn(&self) -> Vec<u8> {
            Vec::new()
        }

        fn encode_on_foot_sync(&self, _sync: &OnFootSync) -> Vec<u8> {
            vec![0xAA, 0xBB]
        }

        fn decode_client_message(
            &self,
            payload: &[u8],
        ) -> samp_proto::Result<samp_proto::ServerMessage> {
            samp_proto::decode_client_message(payload)
        }

        fn decode_player_chat(
            &self,
            payload: &[u8],
        ) -> samp_proto::Result<samp_proto::ChatMessage> {
            samp_proto::decode_player_chat(payload)
        }

        fn encode_chat(&self, text: &[u8]) -> Vec<u8> {
            samp_proto::encode_chat(text)
        }

        fn generate_gpci(&self) -> String {
            "TESTGPCI".to_string()
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

    fn happy_codec() -> FakeCodec {
        FakeCodec {
            connect: (PlayerId(7), ServerCookie(0x1234)),
            init: InitGame {
                local_player_id: PlayerId(42),
                host_name: "Test Server".to_string(),
            },
            class_response: RequestClassResponse {
                allowed: true,
                spawn_position: Vector3 {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                },
                ..RequestClassResponse::default()
            },
            spawn_response: RequestSpawnResponse { allow: 2 },
        }
    }

    fn happy_script() -> Vec<RakEvent> {
        vec![
            RakEvent::Connected { body: Vec::new() },
            RakEvent::Rpc {
                id: RPC_INIT_GAME,
                payload: Vec::new(),
            },
            RakEvent::Rpc {
                id: RPC_REQUEST_CLASS,
                payload: Vec::new(),
            },
            RakEvent::Rpc {
                id: RPC_REQUEST_SPAWN,
                payload: Vec::new(),
            },
        ]
    }

    #[tokio::test]
    async fn reaches_spawned_emitting_events_in_order() {
        let (transport, log) = ScriptedTransport::new(happy_script(), false);
        let mut driver = Driver::new(test_config(), transport, happy_codec());

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
        assert_eq!(
            log.rpc_ids(),
            vec![
                RpcId::ClientJoin as u8,
                RpcId::RequestClass as u8,
                RpcId::RequestSpawn as u8,
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
        let mut driver = Driver::new(test_config(), transport, FakeCodec::default());

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
        let mut driver = Driver::new(test_config(), transport, FakeCodec::default());

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
            crate::state::BotState::shared("Bot".to_string(), "127.0.0.1:7777".parse().unwrap());
        let mut driver = Driver::new(test_config(), transport, FakeCodec::default())
            .with_bot_state(state.clone());

        // GivePlayerMoney (18) is additive; ResetPlayerMoney (20) zeroes.
        driver.track_state(18, &500i32.to_le_bytes());
        driver.track_state(18, &250i32.to_le_bytes());
        assert_eq!(state.borrow().money, 750);
        driver.track_state(20, &[]);
        assert_eq!(state.borrow().money, 0);

        // PutPlayerInVehicle (70): vehicleId u16 then seat u8.
        driver.track_state(70, &[0x2A, 0x00, 0x00]);
        assert_eq!(state.borrow().vehicle, 42);
        driver.track_state(71, &[]); // RemovePlayerFromVehicle
        assert_eq!(state.borrow().vehicle, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn sync_loop_sends_while_spawned() {
        let (transport, log) = ScriptedTransport::new(happy_script(), true);
        let mut driver = Driver::new(test_config(), transport, happy_codec());

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
                Step::SyncTick => driver.on_sync_tick().await,
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
        assert_eq!(packet, vec![SyncPacketId::PlayerSync as u8, 0xAA, 0xBB]);
    }
}
