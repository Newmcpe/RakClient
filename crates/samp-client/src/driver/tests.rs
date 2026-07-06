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
            id: RpcId::InitGame as u8,
            payload: InitGame {
                local_player_id: PlayerId(42),
                host_name: "Test Server".to_string(),
            }
            .encode(),
        },
        RakEvent::Rpc {
            id: RpcId::RequestClass as u8,
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
            id: RpcId::RequestSpawn as u8,
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
    // Server-driven spawn (RakSAMP Lite model): ClientJoin + RequestClass, then the server's
    // RequestSpawnResponse(allow==2) makes us send Spawn. We never send RequestSpawn ourselves here.
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
        vec![RakEvent::Disconnected(DisconnectReason::ConnectionLost)],
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

    assert_eq!(disconnect_message.as_deref(), Some("connection lost"));
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
async fn banned_is_terminal_and_stops() {
    // A ban must NOT reconnect: reconnecting is futile and just hammers the server. The driver closes
    // (next_event yields None) so the app exits cleanly instead of looping.
    let (transport, log) = ScriptedTransport::new(
        vec![RakEvent::Disconnected(DisconnectReason::Banned)],
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

    assert_eq!(disconnect_message.as_deref(), Some("banned"));
    assert!(!driver.reconnect_scheduled());
    assert!(driver.next_event().await.is_none(), "driver should close");
    assert_eq!(log.lock().expect("log poisoned").reconnects, 0);
}

#[tokio::test(start_paused = true)]
async fn repeated_drops_stop_after_cap() {
    // Back-to-back non-terminal drops with no stable session between them (kick loop): the driver must
    // give up after the cap instead of reconnecting forever.
    let script = vec![RakEvent::Disconnected(DisconnectReason::ConnectionLost); 6];
    let (transport, _log) = ScriptedTransport::new(script, false);
    let mut driver = Driver::new(test_config(), transport);

    let mut disconnects = 0usize;
    while let Some(event) = driver.next_event().await {
        if matches!(event, ClientEvent::Disconnected(_)) {
            disconnects += 1;
        }
    }

    // MAX_RECONNECT_ATTEMPTS (5) reconnects are scheduled; the 6th drop trips the cap and closes.
    assert_eq!(disconnects, 6);
    assert!(!driver.reconnect_scheduled());
    assert_eq!(driver.state(), &ConnectionState::Disconnected);
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
    driver.track_state(GivePlayerMoney::RPC_ID, &500i32.to_le_bytes());
    driver.track_state(GivePlayerMoney::RPC_ID, &250i32.to_le_bytes());
    assert_eq!(state.borrow().money, 750);
    driver.track_state(ResetPlayerMoney::RPC_ID, &[]);
    assert_eq!(state.borrow().money, 0);

    // PutPlayerInVehicle (70): vehicleId u16 then seat u8.
    driver.track_state(PutPlayerInVehicle::RPC_ID, &[0x2A, 0x00, 0x00]);
    assert_eq!(state.borrow().vehicle_id(), 42);
    driver.track_state(RemovePlayerFromVehicle::RPC_ID, &[]);
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
