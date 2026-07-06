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
use samp_proto::events::decode_event;
use samp_proto::events::incoming::{
    GivePlayerMoney, InterpolateCamera, PutPlayerInVehicle, RemovePlayerFromVehicle,
    ResetPlayerMoney, SetCameraPosition, SetInterior, SetPlayerPos, SetPlayerPosFindZ,
    SetSpawnInfo, TogglePlayerSpectating,
};
use samp_proto::{
    ArizonaSync221, ChatMessage, ChatOutgoing, ClientJoin, Decode, Direction, Encode, InitGame,
    OnFootSync, OutboundMsg, PlayerId, Quaternion, RequestClass, RequestClassResponse,
    RequestSpawn, RequestSpawnResponse, RpcId, ServerMessage, Spawn, SpectatorSync, StatsUpdate,
    SyncPacketId, Vector3, Verdict, WeaponId, CHALLENGE_XOR, SAMP_VERSION_0_3_7,
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
/// every `sync_interval` tick. Matches the real client's floor: samp.dll's on-foot/aim senders
/// (`sub_10004D40`/`sub_10005040`) force a resend once `GetTickCount - last > 0x1F4` (500 ms) even when
/// the packet is byte-identical, so a stationary player still reports at ≥2 Hz.
const IDLE_SYNC_INTERVAL: Duration = Duration::from_millis(500);

/// How often the client reports its stats (`PACKET_STATS_UPDATE` = 205, money + drunk) while spawned.
/// The real client sends this every second (`NetGame_Process` gate `TickCount - last > 0x3E8`).
const STATS_INTERVAL: Duration = Duration::from_millis(1000);

/// Base offset for the Arizona 221/53 `timestamp_ms` so it resembles a plausible `GetTickCount` uptime
/// (the server only requires it to strictly increase within a session); the elapsed ms are added on.
const ARIZONA_TS_BASE: u32 = 200_000_000;

/// RakNet ordering channel for all player sync (on-foot 207, aim 203, spectator 212). The real client
/// sends sync on channel 1, keeping it off channel 0 which carries the reliable-ordered RPC stream —
/// verified against samp.dll's sync senders (`sub_10004D40`/`sub_10005040`/`sub_10006320`, all
/// `Send(..., orderingChannel = 1)`). We previously sent sync on channel 0, mixing it with RPCs.
const SYNC_CHANNEL: u8 = 1;

/// Arizona's CEF/validation packet family: raw packet id 220, sub-id in the second byte. Logged
/// verbosely in both directions because the Arizona login flow hinges on it.
const ARIZONA_CEF_PACKET_ID: u8 = 220;

/// Arizona's custom streamer-sync packet id (221). Sub-id 53 = our outbound position report; sub-id
/// 113 = the server assigning us the streamer entity id to report under.
const ARIZONA_SYNC_PACKET_ID: u8 = 221;
const ARIZONA_SYNC_ASSIGN_SUB: u8 = 113;

const SPAWN_HEALTH: u8 = 100;

/// Fallback spawn position for [`Driver::enter_spawned`] when the server never sent `SetSpawnInfo`
/// (the self-spawn fallback path). Decoded from a real captured `SetSpawnInfo` body on Arizona's
/// Bumble Bee server (`[u8 team=255][u32 skin][u8 unused][f32 x,y,z,zAngle]...`, floats at byte
/// offsets 6/10/14/18) rather than defaulting to the map origin.
const FALLBACK_SPAWN_POSITION: Vector3 = Vector3 {
    x: 1765.50,
    y: -1892.70,
    z: 13.56,
};

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
    /// When the periodic `PACKET_STATS_UPDATE` (205) was last sent; drives the 1 Hz stats cadence.
    last_stats_at: Option<Instant>,
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
    /// Position from `SetSpawnInfo` (RPC 68); `enter_spawned` loads it into the sync position,
    /// mirroring RakSAMP Lite's `Net_Spawn` — without this the bot reports 0,0,0 after spawn.
    spawn_info_position: Option<Vector3>,
    /// When the pre-spawn window began — the class response or the server's spectate-on toggle,
    /// whichever lands first; drives the optional self-spawn fallback (`config.self_spawn_timeout`).
    class_selected_at: Option<Instant>,
    /// Driver-creation instant; the monotonic base for the Arizona 221/53 `timestamp_ms` field.
    created_at: Instant,
    /// Streamer entity id the server assigns us via inbound `221/113` (`[0xDD][0x71][0x00][u16 id]`).
    /// Our outbound `221/53` sync must report THIS id, not our SA-MP player id, or the server's
    /// streamer watchdog treats the entity's sync as ignored ("Игнорирование функции(52 / 1)").
    arizona_streamer_id: Option<u16>,
    /// Consecutive reconnect attempts since the last stable session. Caps a kick/reject loop so a
    /// server that keeps dropping us right back out doesn't spin the client forever.
    reconnect_attempts: u32,
    /// When we last reached a stable in-game state (`Spawned`). A session that survived at least
    /// [`STABLE_SESSION`] before dropping resets the attempt cap — a genuine one-off disconnect
    /// reconnects freely, while a spawn→instant-kick loop keeps accumulating toward the cap.
    connected_since: Option<Instant>,
}

/// Give up reconnecting after this many consecutive attempts without a stable session — the drop is a
/// kick/ban/block that reconnecting won't fix, so stop instead of looping.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;
/// A session must last at least this long (spawned) to count as "stable" and reset the attempt cap.
const STABLE_SESSION: Duration = Duration::from_secs(60);

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
            last_stats_at: None,
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
            spawn_info_position: None,
            class_selected_at: None,
            created_at: Instant::now(),
            arizona_streamer_id: None,
            reconnect_attempts: 0,
            connected_since: None,
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
        }
    }

    /// Send everything scripts queued via `sampSendPacket`/`sampSendRpc` since the last drain, each
    /// with the reliability/channel the script asked for (`sendPacket()` defaults to
    /// reliable-ordered on channel 0 — the Arizona `220` path). Outgoing packets pass through the
    /// `onSendPacket` chokepoint first.
    async fn flush_outbox(&mut self) {
        for msg in self.registry.drain_outbox() {
            let msg = match msg {
                OutboundMsg::Packet {
                    data,
                    reliability,
                    channel,
                } => {
                    let id = data.first().copied().unwrap_or(0);
                    let data = match self
                        .registry
                        .dispatch_packet(Direction::Outgoing, id, &data)
                    {
                        Verdict::Drop => continue,
                        Verdict::Pass => data,
                        Verdict::Rewrite(bytes) => bytes,
                    };
                    if id == ARIZONA_CEF_PACKET_ID {
                        tracing::debug!(
                            sub_id = data.get(1).copied().unwrap_or(0),
                            channel,
                            reliability,
                            payload = %hex(&data),
                            "outbound 220",
                        );
                    }
                    OutboundMsg::Packet {
                        data,
                        reliability,
                        channel,
                    }
                }
                OutboundMsg::Rpc { id, payload } => {
                    tracing::debug!(rpc_id = id, payload = %hex(&payload), "outbound RPC");
                    OutboundMsg::Rpc { id, payload }
                }
            };
            if let Err(error) = self.send_outbound_msg(msg).await {
                tracing::warn!(%error, "failed to send script-queued message");
                self.on_disconnect(DisconnectReason::ConnectionLost);
                return;
            }
        }
    }

    /// Send one queued wire message with the reliability/channel it carries.
    async fn send_outbound_msg(&mut self, msg: OutboundMsg) -> raknet::Result<()> {
        match msg {
            OutboundMsg::Packet {
                data,
                reliability,
                channel,
            } => {
                self.transport
                    .send(data, Reliability::from_wire(reliability), channel)
                    .await
            }
            OutboundMsg::Rpc { id, payload } => self.transport.rpc(id, payload).await,
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
            // Script-driven keypresses (e.g. an Arizona ALT interaction): the server derives
            // OnPlayerKeyStateChange from this field, so a script pulses `setBotKeys(mask)` then 0.
            self.sync.keys = s.on_foot.keys;
            self.sync.health = s.on_foot.health;
            self.sync.armour = s.on_foot.armour;
            self.sync.weapon = WeaponId(s.on_foot.weapon);
            self.sync.move_speed = s.on_foot.move_speed;
            self.sync.animation_id = s.on_foot.animation_id;
            self.sync.animation_flags = s.on_foot.animation_flags;
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
            self.request_spawn().await;
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
        // Surface incoming dialogs in the console at info level (like server chat), independent of any
        // script — so every ShowDialog the server sends is visible without enabling debug logging.
        if id == RpcId::ShowDialog as u8 {
            if let Ok(d) = samp_proto::ShowDialog::decode(payload) {
                // Body holds the info text / list rows (options) for list/tablist dialogs; show them
                // indented under the header, one per line, so list dialogs are readable in the console.
                let body = samp_proto::decode_cp1251(&samp_proto::ShowDialog::body(payload));
                let options: String = body
                    .split('\n')
                    .filter(|row| !row.trim().is_empty())
                    .map(|row| format!("\n    • {}", row.trim_end()))
                    .collect();
                tracing::info!(
                    "dialog #{} (style {}): {:?}  [{} | {}]{}",
                    d.dialog_id,
                    d.style,
                    samp_proto::decode_cp1251(&d.title),
                    samp_proto::decode_cp1251(&d.button1),
                    samp_proto::decode_cp1251(&d.button2),
                    options,
                );
            }
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
        match (RpcId::try_from(id), &self.state) {
            (Ok(RpcId::InitGame), ConnectionState::Joining) => self.on_init_game(payload).await,
            // We re-request class every second pre-spawn, so class responses arrive in both
            // ClassSelection and ClassSelected; once we have spawn info the response requests spawn.
            (
                Ok(RpcId::RequestClass),
                ConnectionState::ClassSelection | ConnectionState::ClassSelected,
            ) => self.on_class_response(payload).await,
            // The server drives the spawn with `RequestSpawnResponse` while we're spectating
            // (ClassSelection/ClassSelected), or it answers an explicit `sampSpawnPlayer()` RequestSpawn.
            (
                Ok(RpcId::RequestSpawn),
                ConnectionState::ClassSelection | ConnectionState::ClassSelected,
            ) => self.on_spawn_response(payload).await,
            (Ok(RpcId::TogglePlayerSpectating), _) => self.on_toggle_spectating(payload).await,
            (Ok(RpcId::SetSpawnInfo), _) => self.on_set_spawn_info(payload).await,
            (Ok(RpcId::ConnectionRejected), _) => self.on_disconnect(DisconnectReason::Rejected),
            (Ok(RpcId::ClientMessage), _) => self.on_client_message(payload),
            (Ok(RpcId::Chat), _) => self.on_player_chat(payload),
            _ => {}
        }
    }

    /// Update the shared local-player state from money/interior/vehicle/camera RPCs so `getBotMoney`
    /// etc. return live values. Unknown ids and undecodable payloads are ignored.
    fn track_state(&mut self, id: u8, payload: &[u8]) {
        use crate::state::InVehicleData;
        let Some(state) = &self.bot_state else {
            return;
        };
        let mut s = state.borrow_mut();
        match id {
            GivePlayerMoney::RPC_ID => {
                if let Ok(ev) = decode_event::<GivePlayerMoney>(payload) {
                    s.money += ev.money; // GivePlayerMoney is additive
                }
            }
            ResetPlayerMoney::RPC_ID => s.money = 0,
            SetInterior::RPC_ID => {
                if let Ok(ev) = decode_event::<SetInterior>(payload) {
                    s.interior = ev.interior;
                }
            }
            PutPlayerInVehicle::RPC_ID => {
                if let Ok(ev) = decode_event::<PutPlayerInVehicle>(payload) {
                    s.vehicle = Some(InVehicleData {
                        id: ev.vehicle_id,
                        seat: ev.seat_id,
                        ..InVehicleData::default()
                    });
                }
            }
            RemovePlayerFromVehicle::RPC_ID => s.vehicle = None,
            SetCameraPosition::RPC_ID => {
                if let Ok(ev) = decode_event::<SetCameraPosition>(payload) {
                    s.camera_pos = ev.position;
                }
            }
            _ => {}
        }
    }

    /// Follow server repositions for aim-sync: `SetPlayerPos(FindZ)` while spawned, and the camera
    /// RPCs before spawn, move the bot and regenerate the aim (ported from `aim_fix_updated.lua`).
    fn aim_follow(&mut self, id: u8, payload: &[u8]) {
        let spawned = matches!(self.state, ConnectionState::Spawned);
        let new_pos = match id {
            SetPlayerPos::RPC_ID if spawned => decode_event::<SetPlayerPos>(payload)
                .ok()
                .map(|ev| ev.position),
            SetPlayerPosFindZ::RPC_ID if spawned => decode_event::<SetPlayerPosFindZ>(payload)
                .ok()
                .map(|ev| ev.position),
            SetCameraPosition::RPC_ID if !spawned => decode_event::<SetCameraPosition>(payload)
                .ok()
                .map(|ev| ev.position),
            // `InterpolateCamera` moves us only when it carries a destination position.
            InterpolateCamera::RPC_ID if !spawned => decode_event::<InterpolateCamera>(payload)
                .ok()
                .and_then(|ev| ev.set_pos.then_some(ev.dest_pos)),
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
            if let Err(error) = self.send_outbound_msg(msg).await {
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
        // Arizona 221/113 assigns our streamer entity id — capture it for the outbound 221/53 sync.
        if id == ARIZONA_SYNC_PACKET_ID && data.get(1) == Some(&ARIZONA_SYNC_ASSIGN_SUB) {
            if let (Some(&lo), Some(&hi)) = (data.get(3), data.get(4)) {
                let entity = u16::from_le_bytes([lo, hi]);
                if self.arizona_streamer_id != Some(entity) {
                    tracing::info!(entity, "arizona: assigned streamer entity id (221/113)");
                }
                self.arizona_streamer_id = Some(entity);
            }
        }
        if id == ARIZONA_CEF_PACKET_ID {
            tracing::debug!(
                sub_id = data.get(1).copied().unwrap_or(0),
                payload = %hex(data),
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
        let Some(init) = decode_or_warn::<InitGame>(payload, "init game") else {
            return;
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

    /// After join, spectate and start the pre-spawn poll. Like RakSAMP Lite (`client_app.cpp`) we send
    /// `RequestClass` now and re-send it every second (plus the score/ping) until the server sends
    /// `SetSpawnInfo`; the spawn is then driven by the class response (with spawn info),
    /// `RequestSpawnResponse(allow==2)`, or `TogglePlayerSpectating(1→0)`.
    async fn request_spawn_class(&mut self) {
        self.spectating = true;
        self.transition(ConnectionState::ClassSelection);
        let payload = RequestClass {
            class: self.config.default_class,
        }
        .encode();
        self.send_rpc(RpcId::RequestClass as u8, payload).await;
    }

    /// Send `RequestSpawn` (RPC) and mark that we asked, so a non-`allow==2` response still spawns us.
    async fn request_spawn(&mut self) {
        self.spawn_requested = true;
        let payload = RequestSpawn.encode();
        self.send_rpc(RpcId::RequestSpawn as u8, payload).await;
    }

    /// Pre-spawn tick while spectating: only the optional self-spawn fallback runs here. A real
    /// client sends neither a periodic `RequestClass` re-request nor an automatic `UpdateScoresPingsIPs`
    /// heartbeat while waiting to spawn (live capture: `RequestClass` never appears in a real client's
    /// outbound traffic at all, and `UpdateScoresPingsIPs` only fires once, user-triggered by opening
    /// the scoreboard, hours into a session — never automatically near join). Sending either on a
    /// steady 1s cadence during the exact window the server decides whether to grant a spawn is a
    /// distinctive non-human traffic pattern a server could key an anti-bot check on, so we don't.
    async fn send_prespawn_poll(&mut self) {
        // The self-spawn fallback (`config.self_spawn_timeout`) is opt-in and OFF by default. On
        // Arizona an unauthorised RPC_Spawn (no server `SetSpawnInfo`) trips the anti-cheat, which
        // kicks with "подозрение в читерстве" ~60s later — verified live. A spectating client is not
        // flagged and still receives chat/world state, so staying spectating is the safe default; the
        // fallback exists only for non-Arizona servers that legitimately never drive the spawn.
        let Some(timeout) = self.config.self_spawn_timeout else {
            return;
        };
        if let Some(t) = self.class_selected_at {
            if Instant::now().duration_since(t) >= timeout {
                tracing::warn!(
                    "server never drove the spawn — spawning without server authorisation"
                );
                self.spectating = false;
                self.enter_spawned().await;
            }
        }
    }

    async fn on_class_response(&mut self, payload: &[u8]) {
        let Some(response) = decode_or_warn::<RequestClassResponse>(payload, "class response")
        else {
            return;
        };
        if response.allowed {
            self.sync.position = response.spawn_position;
            self.mirror_to_state();
        }
        self.transition(ConnectionState::ClassSelected);
        self.class_selected_at.get_or_insert_with(Instant::now);
        // RakSAMP Lite (`network_transport.cpp`): on a class response, if the server has already sent
        // `SetSpawnInfo` (i.e. login is done), request spawn now — the server answers with
        // `RequestSpawnResponse`, which spawns us. Before spawn info we keep spectating (requesting
        // spawn early earns "ОШИБКА 7721").
        if self.spawn_info_position.is_some() && self.spectating {
            self.request_spawn().await;
        }
    }

    async fn on_spawn_response(&mut self, payload: &[u8]) {
        let Some(response) = decode_or_warn::<RequestSpawnResponse>(payload, "spawn response")
        else {
            return;
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

    /// `SetSpawnInfo` (RPC 68): remember the spawn position for `enter_spawned`, like RakSAMP Lite's
    /// `has_spawn_info` + `Net_Spawn` position copy.
    async fn on_set_spawn_info(&mut self, payload: &[u8]) {
        let Some(info) = decode_event_or_warn::<SetSpawnInfo>(payload, "spawn info") else {
            return;
        };
        let pos = info.position;
        tracing::debug!(x = pos.x, y = pos.y, z = pos.z, "spawn info position");
        self.spawn_info_position = Some(pos);
        // The real client answers EVERY SetSpawnInfo with RPC 52 (Spawn) — including the SECOND one the
        // server sends right after a fresh account confirms its skin (`chooseSelector.buy`). That second
        // Spawn is what moves the character out of the ChooseSelector room (interior 211) into the world;
        // NOT sending it is exactly the "Игнорирование функции(52 / 1)" anti-cheat kick. The FIRST
        // SetSpawnInfo arrives while still spectating (not yet Spawned) and is driven by the normal
        // class/spawn-response path, so only re-send when we're ALREADY Spawned (the post-skin / respawn
        // case). Re-send Spawn and adopt the new position, but don't re-fire the Spawned event — the FSM
        // is already Spawned and the scripts' onSpawn already ran.
        if matches!(self.state, ConnectionState::Spawned) {
            let spawn = Spawn.encode();
            self.send_rpc(RpcId::Spawn as u8, spawn).await;
            self.sync.position = pos;
            self.mirror_to_state();
            tracing::info!(
                x = pos.x,
                y = pos.y,
                z = pos.z,
                "re-spawn (RPC 52) on post-skin SetSpawnInfo"
            );
        }
    }

    /// `TogglePlayerSpectating` (RPC 124): the Arizona server toggles spectate ON during login and
    /// OFF afterwards. A `1 → 0` transition means "drop out of spectate" → spawn, exactly like
    /// RakSAMP Lite's `sub_45B260 → Net_Spawn`. This is the server-driven spawn, no explicit call.
    async fn on_toggle_spectating(&mut self, payload: &[u8]) {
        let Some(toggle) =
            decode_event_or_warn::<TogglePlayerSpectating>(payload, "spectating toggle")
                .map(|ev| ev.state)
        else {
            return;
        };
        let was_spectating = self.server_spectating;
        self.server_spectating = toggle;
        if toggle {
            // Arms the self-spawn fallback timer even when no class response will ever arrive
            // (an Arizona script drops the automatic RequestClass, so the server's spectate-on is
            // then the only signal that the pre-spawn window has begun).
            self.class_selected_at.get_or_insert_with(Instant::now);
        }
        if was_spectating && !toggle && !matches!(self.state, ConnectionState::Spawned) {
            // RakSAMP Lite (`network_transport.cpp`): a `1 → 0` spectate toggle routes through
            // `RequestSpawn` (RPC 129), NOT a direct `Spawn` (RPC 52). The server then replies
            // `RequestSpawnResponse` with `allow != 0`, which drives `enter_spawned`. Sending an
            // unsolicited `Spawn` (skipping the RequestSpawn→allow handshake) is what Arizona's
            // anti-cheat flags as "Игнорирование функции(52 / 1)" and kicks ~16s after spawn. Stay
            // spectating until the server's spawn grant arrives.
            self.request_spawn().await;
        }
    }

    /// Send `RPC_Spawn` and enter the `Spawned` state — the single convergence point for every spawn
    /// trigger. We spectate (spectator-sync 212) after join until one of these fires (the only
    /// triggers the real RakSAMP Lite binary has — `Net_Spawn` xrefs at `0x455bb0`):
    /// - `TogglePlayerSpectating` (RPC 124) on a `1 → 0` transition (`on_toggle_spectating`) — the
    ///   Arizona post-login trigger — or `RequestSpawnResponse(allow==2)` (`on_spawn_response`).
    /// - MANUAL: `sampSpawnPlayer()` sends `RequestSpawn`, whose response then spawns us.
    async fn enter_spawned(&mut self) {
        let payload = Spawn.encode();
        self.send_rpc(RpcId::Spawn as u8, payload).await;
        // Net_Spawn copies the SetSpawnInfo position into the local sync position — this is where the
        // post-spawn position comes from (the class-response spawn_position is usually zero).
        match self.spawn_info_position {
            Some(pos) => self.sync.position = pos,
            None => {
                tracing::warn!(
                    "spawning without SetSpawnInfo — using the captured fallback position"
                );
                self.sync.position = FALLBACK_SPAWN_POSITION;
            }
        }
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

    /// Send a spectator-sync packet (212) — the pre-spawn keepalive while spectating.
    async fn send_spectator_sync(&mut self) {
        let packet = SpectatorSync {
            position: self.sync.position,
        }
        .to_packet();
        // The real client sends spectator sync UNRELIABLE (reliability 6, not sequenced) on the sync
        // channel — samp.dll `sub_10006320`, `Send(..., reliability = 6, orderingChannel = 1)`.
        if let Err(error) = self
            .transport
            .send(packet, Reliability::Unreliable, SYNC_CHANNEL)
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

    /// One sync cycle: pre-spawn it spectates, spawned it runs the on-foot/weapons/aim sends. Any
    /// transport failure tears the connection down once.
    async fn on_sync_tick(&mut self, force: bool) {
        // Arizona pre-spawn: spectate (keepalive) while awaiting the login dialog, not on-foot sync.
        if self.spectating && !matches!(self.state, ConnectionState::Spawned) {
            self.send_spectator_sync().await;
            self.send_prespawn_poll().await;
            return;
        }
        if self.run_spawned_sync(force).await.is_err() {
            self.on_disconnect(DisconnectReason::ConnectionLost);
        }
    }

    /// The spawned-state sends, each on its own cadence: adaptive on-foot sync, the periodic weapon
    /// inventory, then aim. Bails on the first transport error (the caller disconnects).
    async fn run_spawned_sync(&mut self, force: bool) -> raknet::Result<()> {
        self.mirror_from_state();
        self.adjust_on_foot_from_emulation();
        self.send_on_foot_sync(force).await?;
        self.send_arizona_221_sync().await?;
        self.send_weapons_update().await?;
        self.send_stats_update().await?;
        self.send_aim_sync().await
    }

    /// Stream Arizona's custom on-foot position report (packet 221, sub-id 53) alongside stock 207.
    /// Arizona's anti-cheat kicks a client that never sends it ("Игнорирование функции(52 / 1)"). This
    /// is the minimal stationary form: our own entity id + current position + a monotonic ms timestamp
    /// + the rest velocity/heading. Sent `UnreliableSequenced` on [`SYNC_CHANNEL`] like the other sync.
    async fn send_arizona_221_sync(&mut self) -> raknet::Result<()> {
        // Report the server-assigned streamer id (from 221/113); fall back to our player id until it
        // arrives. Reporting the wrong id is what the streamer watchdog flags as "function 52".
        let packet = ArizonaSync221 {
            entity_id: self.arizona_streamer_id.unwrap_or(self.local_id.0),
            position: self.sync.position,
            timestamp_ms: ARIZONA_TS_BASE
                .wrapping_add(self.created_at.elapsed().as_millis() as u32),
            velocity: ArizonaSync221::REST_VELOCITY,
            heading: ArizonaSync221::REST_HEADING,
        }
        .encode();
        self.transport
            .send(packet, Reliability::UnreliableSequenced, SYNC_CHANNEL)
            .await
            .inspect_err(|error| tracing::warn!(%error, "failed to send Arizona 221 sync"))
    }

    /// Report player stats (`PACKET_STATS_UPDATE` = 205: money + drunk) at the [`STATS_INTERVAL`]
    /// cadence, matching the real client's 1 Hz stats send while spawned. Sent UNRELIABLE on channel 0
    /// like the real client (`NetGame_Process` @0x10005B10); drunk level is always 0 (headless bot).
    async fn send_stats_update(&mut self) -> raknet::Result<()> {
        let now = Instant::now();
        if self
            .last_stats_at
            .is_some_and(|at| now.duration_since(at) < STATS_INTERVAL)
        {
            return Ok(());
        }
        let money = self
            .bot_state
            .as_ref()
            .map_or(0, |state| state.borrow().money);
        let packet = StatsUpdate {
            money,
            drunk_level: 0,
        }
        .to_packet();
        self.transport
            .send(packet, Reliability::Unreliable, 0)
            .await
            .inspect_err(|error| tracing::warn!(%error, "failed to send stats update"))?;
        self.last_stats_at = Some(now);
        Ok(())
    }

    /// Emulation adjustments folded into the outgoing on-foot state: report the held weapon and the
    /// occasional score-ping key blip.
    fn adjust_on_foot_from_emulation(&mut self) {
        if let Some(state) = self.bot_state.as_ref() {
            let (weapon, keys) =
                self.emulation
                    .adjust_on_foot(&state.borrow(), self.sync.keys, Instant::now());
            self.sync.weapon = WeaponId(weapon);
            self.sync.keys = keys;
        }
    }

    /// The on-foot packet is sent adaptively — on any state change, on a forced sync
    /// (`updateSync()`), or otherwise only at the [`IDLE_SYNC_INTERVAL`] keepalive cadence — so a
    /// stationary bot stops flooding identical packets.
    async fn send_on_foot_sync(&mut self, force: bool) -> raknet::Result<()> {
        let now = Instant::now();
        let due = force
            || self.sync != self.last_sync
            || self
                .last_sync_at
                .is_none_or(|at| now.duration_since(at) >= IDLE_SYNC_INTERVAL);
        if !due {
            return Ok(());
        }
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
            self.transport
                .send(packet, Reliability::UnreliableSequenced, SYNC_CHANNEL)
                .await
                .inspect_err(|error| tracing::warn!(%error, "failed to send on-foot sync"))?;
            self.last_sync = self.sync;
            self.last_sync_at = Some(now);
        }
        Ok(())
    }

    /// Emulation: stream the weapon inventory periodically.
    async fn send_weapons_update(&mut self) -> raknet::Result<()> {
        let msg = match self.bot_state.as_ref() {
            Some(state) => self
                .emulation
                .due_weapons_update(&state.borrow(), Instant::now()),
            None => None,
        };
        match msg {
            Some(msg) => self
                .send_outbound_msg(msg)
                .await
                .inspect_err(|error| tracing::warn!(%error, "failed to send weapons update")),
            None => Ok(()),
        }
    }

    /// Native aim-sync: note the position (to detect movement) and send a believable aim when due.
    async fn send_aim_sync(&mut self) -> raknet::Result<()> {
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
        match aim_packet {
            Some(packet) => self
                .transport
                .send(packet, Reliability::UnreliableSequenced, SYNC_CHANNEL)
                .await
                .inspect_err(|error| tracing::warn!(%error, "failed to send aim sync")),
            None => Ok(()),
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
                self.last_stats_at = None;
                self.spawn_info_position = None;
                self.class_selected_at = None;
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
            .push_back(ClientEvent::Disconnected(reason.to_string()));

        // Local quit: never reconnect.
        if reason == DisconnectReason::Local {
            return;
        }

        // Terminal reasons: reconnecting is futile, and for a ban it just hammers the server. Stop the
        // client cleanly (closed → next_event yields None → the app loop exits) instead of looping.
        if matches!(
            reason,
            DisconnectReason::Banned | DisconnectReason::InvalidPassword
        ) {
            tracing::warn!(reason = %reason, "terminal disconnect — not reconnecting; stopping");
            self.reconnect_timer = None;
            self.closed = true;
            return;
        }

        // Everything else (kick / rejection / connection loss / timeout) is retried, but with a cap so a
        // server that keeps dropping us straight back out can't spin forever. A session that survived at
        // least STABLE_SESSION before dropping is treated as a one-off and resets the counter; a
        // never-stabilising loop (kicked before/at spawn, or spawn→instant-kick) keeps accumulating.
        let was_stable = self
            .connected_since
            .is_some_and(|since| since.elapsed() >= STABLE_SESSION);
        if was_stable {
            self.reconnect_attempts = 0;
        }
        self.connected_since = None;
        self.reconnect_attempts += 1;
        if self.reconnect_attempts > MAX_RECONNECT_ATTEMPTS {
            tracing::warn!(
                attempts = self.reconnect_attempts,
                reason = %reason,
                "giving up after repeated reconnects (kicked/blocked) — stopping"
            );
            self.reconnect_timer = None;
            self.closed = true;
            return;
        }
        tracing::info!(
            attempt = self.reconnect_attempts,
            max = MAX_RECONNECT_ATTEMPTS,
            reason = %reason,
            "scheduling reconnect"
        );
        self.schedule_reconnect();
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
        let payload = ChatOutgoing { text }.encode();
        self.send_rpc(RpcId::Chat as u8, payload).await;
    }

    fn transition(&mut self, next: ConnectionState) {
        self.state = next.clone();
        // Reaching Spawned is a stable milestone: start the session clock so a long, healthy session
        // resets the reconnect-attempt cap when it eventually drops (see on_disconnect).
        if matches!(self.state, ConnectionState::Spawned) {
            self.connected_since = Some(Instant::now());
        }
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

/// Decode a `Decode` packet body, warning and returning `None` on failure.
fn decode_or_warn<T: Decode>(payload: &[u8], what: &'static str) -> Option<T> {
    match T::decode(payload) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, "failed to decode {what}");
            None
        }
    }
}

/// Decode a `WireDecode` event body, warning and returning `None` on failure.
fn decode_event_or_warn<T: samp_proto::events::WireDecode>(
    payload: &[u8],
    what: &'static str,
) -> Option<T> {
    match decode_event::<T>(payload) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, "failed to decode {what}");
            None
        }
    }
}

/// Render a byte slice as contiguous lowercase hex for wire-level debug logs.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode an inbound RPC body into a human-readable struct for logging — turns the raw bytes into
/// named fields (cp1251 text rendered as strings). Covers the dialog/chat specials plus everything in
/// the typed incoming registry; returns `None` for ids with no decoder.
fn decode_inbound_rpc(id: u8, payload: &[u8]) -> Option<String> {
    use samp_proto::events::FieldValue as F;
    match RpcId::try_from(id) {
        Ok(RpcId::ShowDialog) => samp_proto::ShowDialog::decode(payload).ok().map(|d| {
            format!(
                "ShowDialog {{ dialog_id: {}, style: {}, title: {:?}, button1: {:?}, button2: {:?} }}",
                d.dialog_id,
                d.style,
                samp_proto::decode_cp1251(&d.title),
                samp_proto::decode_cp1251(&d.button1),
                samp_proto::decode_cp1251(&d.button2),
            )
        }),
        Ok(RpcId::ClientMessage) => samp_proto::ServerMessage::decode(payload).ok().map(|m| {
            format!(
                "ClientMessage {{ color: {:08X}, text: {:?} }}",
                m.color,
                samp_proto::decode_cp1251(&m.text)
            )
        }),
        Ok(RpcId::Chat) => samp_proto::ChatMessage::decode(payload).ok().map(|m| {
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

#[cfg(test)]
mod tests;
