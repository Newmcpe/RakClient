//! SA-MP connection state machine and high-level [`Client`].
//!
//! Drives the reversed connect → play sequence over the [`raknet`] transport using [`samp_proto`]
//! codecs:
//!
//! ```text
//! Disconnected → Connecting → RakNetConnected → Joining → Joined
//!   → ClassSelection → ClassSelected → Spawned
//! ```
//!
//! The crate root holds the public contract; the FSM lives in the private [`driver`] module driven
//! over the [`transport`] seam.
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::time::Duration;

use samp_proto::{ClassId, PlayerId};
use thiserror::Error;

mod aim;
mod client_emulation;
mod driver;
mod registry;
mod state;
mod transport;
mod type3;

pub use registry::{Action, PacketRegistry};
pub use samp_proto::{Direction, OutboundMsg, Outbox, Verdict};
pub use state::{
    AimData, InVehicleData, LocalPlayer, OnFootData, SharedLocalPlayer, WeaponInventory,
};

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Raknet(#[from] raknet::RaknetError),
    #[error(transparent)]
    Proto(#[from] samp_proto::ProtoError),
    #[error("disconnected: {0:?}")]
    Disconnected(raknet::DisconnectReason),
}

/// Everything needed to connect and reach `Spawned`.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server: SocketAddr,
    pub nick: String,
    /// RakNet connection password (the server-level password sent in `CONNECTION_REQUEST`). `None`
    /// for open servers like Arizona — this is *not* the account login password.
    pub password: Option<String>,
    pub client_version: String,
    pub default_class: ClassId,
    /// gpci/auth token; `None` ⇒ generate a best-effort one.
    pub gpci: Option<String>,
    pub sync_interval: Duration,
    pub reconnect_delay: Duration,
}

impl ClientConfig {
    pub fn builder(server: SocketAddr, nick: impl Into<String>) -> ClientConfigBuilder {
        ClientConfigBuilder {
            config: ClientConfig {
                server,
                nick: nick.into(),
                password: None,
                client_version: "0.3.7-R3".to_string(),
                default_class: ClassId::default(),
                gpci: None,
                sync_interval: Duration::from_millis(100),
                reconnect_delay: Duration::from_secs(5),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClientConfigBuilder {
    config: ClientConfig,
}

impl ClientConfigBuilder {
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.config.password = Some(password.into());
        self
    }
    pub fn client_version(mut self, version: impl Into<String>) -> Self {
        self.config.client_version = version.into();
        self
    }
    pub fn default_class(mut self, class: ClassId) -> Self {
        self.config.default_class = class;
        self
    }
    pub fn gpci(mut self, gpci: impl Into<String>) -> Self {
        self.config.gpci = Some(gpci.into());
        self
    }
    pub fn sync_interval(mut self, interval: Duration) -> Self {
        self.config.sync_interval = interval;
        self
    }
    pub fn reconnect_delay(mut self, delay: Duration) -> Self {
        self.config.reconnect_delay = delay;
        self
    }
    pub fn build(self) -> ClientConfig {
        self.config
    }
}

/// The connection state machine — a data-carrying enum, not a bag of bool flags.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    RakNetConnected,
    Joining,
    Joined {
        local_id: PlayerId,
        host_name: String,
    },
    ClassSelection,
    ClassSelected,
    Spawned,
}

/// Events emitted to consumers of the client.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    StateChanged(ConnectionState),
    Connected,
    Joined {
        local_id: PlayerId,
        host_name: String,
    },
    Spawned,
    /// A coloured server/system text line (`RPC_ClientMessage`). `text` is already decoded from
    /// cp1251; `color` is the wire `0xRRGGBBAA`.
    ServerMessage {
        color: u32,
        text: String,
    },
    /// A player chat broadcast (`RPC_Chat`). `text` is already decoded from cp1251.
    Chat {
        player_id: PlayerId,
        text: String,
    },
    Disconnected(String),
}

/// High-level async SA-MP client. Owns a [`raknet::RakHandle`] and drives the FSM.
pub struct Client {
    driver: driver::Driver<transport::RakTransport>,
}

impl Client {
    /// Connect and start driving the FSM. Returns once the transport task is running.
    pub async fn connect(config: ClientConfig) -> Result<Self> {
        let rak_config = raknet::RakConfig {
            password: config.password.clone(),
            static_data: Vec::new(),
        };
        let transport = transport::RakTransport::connect(config.server, rak_config).await?;
        let driver = driver::Driver::new(config, transport);
        Ok(Self { driver })
    }

    /// Connect with a [`PacketRegistry`] attached: registered handlers (scripts/observers) intercept
    /// every incoming/outgoing RPC before the FSM, and `on_update` handlers fire on the driver's
    /// update tick. The registry holds non-`Send` script closures, so a client built this way is
    /// itself `!Send` — drive it inline (do not `tokio::spawn` it).
    pub async fn connect_with_registry(
        config: ClientConfig,
        registry: PacketRegistry,
        bot_state: SharedLocalPlayer,
    ) -> Result<Self> {
        let rak_config = raknet::RakConfig {
            password: config.password.clone(),
            static_data: Vec::new(),
        };
        let transport = transport::RakTransport::connect(config.server, rak_config).await?;
        let driver = driver::Driver::new(config, transport)
            .with_registry(registry)
            .with_bot_state(bot_state);
        Ok(Self { driver })
    }

    /// Current connection state.
    pub fn state(&self) -> &ConnectionState {
        self.driver.state()
    }

    /// Pump the state machine, yielding the next client-facing event. `None` when closed.
    pub async fn next_event(&mut self) -> Option<ClientEvent> {
        self.driver.next_event().await
    }

    /// Send a chat line as the local player (`RPC_Chat`). `text` is raw bytes in the server's
    /// encoding — for Cyrillic (Arizona) servers, encode to cp1251 first; ASCII passes through. The
    /// length is a single byte so `text` is truncated to 255 bytes.
    pub async fn send_chat(&mut self, text: &[u8]) {
        self.driver.send_chat(text).await;
    }

    /// Gracefully disconnect, sending a `DISCONNECTION_NOTIFICATION` and closing the transport.
    pub async fn disconnect(mut self) -> Result<()> {
        self.driver.disconnect().await?;
        Ok(())
    }
}

#[cfg(test)]
mod e2e_tests {
    use super::*;

    use test_support::MockSampServer;

    /// Full stack over loopback UDP: the real [`raknet`] transport drives the connect → spawn
    /// handshake against [`MockSampServer`], which frames its replies through the same
    /// [`raknet::wire`] primitives. Each phase is wrapped in a [`tokio::time::timeout`] so a future
    /// wire-framing regression fails fast instead of hanging.
    #[tokio::test]
    #[ignore = "e2e: real loopback raknet transport + mock server; run with --include-ignored"]
    async fn end_to_end_reaches_spawned() {
        let server = MockSampServer::start().await.expect("start mock server");
        let config = ClientConfig::builder(server.local_addr(), "E2ETester")
            .sync_interval(Duration::from_millis(50))
            .build();

        let mut client = Client::connect(config).await.expect("connect");

        let reached_spawned = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = client.next_event().await {
                if matches!(event, ClientEvent::Spawned) {
                    return true;
                }
            }
            false
        })
        .await
        .expect("e2e timed out before reaching Spawned");
        assert!(reached_spawned, "client should reach Spawned");
        assert_eq!(client.state(), &ConnectionState::Spawned);

        // Keep pumping the FSM so the on-foot sync timer fires, and wait for the mock to record at
        // least one sync packet. `next_event` ticks syncs internally without yielding an event, and a
        // `Client` is `!Send` (it may carry script handlers), so the pump runs concurrently on this
        // task via `select!` rather than a spawned task — the wait branch wins once a sync lands.
        let got_sync = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                _ = async { while client.next_event().await.is_some() {} } => {}
                _ = async {
                    while server.sync_packets_received() == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => {}
            }
        })
        .await;

        assert!(
            got_sync.is_ok(),
            "mock never received an on-foot sync packet"
        );
        assert!(server.sync_packets_received() > 0);
    }
}
