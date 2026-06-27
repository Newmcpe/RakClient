//! Transport seam between the FSM [`crate::driver::Driver`] and the RakNet layer.
//!
//! The [`Transport`] trait abstracts [`raknet::RakHandle`] plus its event receiver so the FSM can be
//! driven by a scripted fake in unit tests (the real transport is exercised by the ignored
//! end-to-end test once the sibling crates are implemented).

use std::net::SocketAddr;

use raknet::{RakConfig, RakEvent, RakHandle, RakPeer, Reliability, Result as RakResult};
use tokio::sync::mpsc;

pub(crate) trait Transport {
    async fn send(&self, data: Vec<u8>, reliability: Reliability, channel: u8) -> RakResult<()>;
    async fn rpc(&self, rpc_id: u8, payload: Vec<u8>) -> RakResult<()>;
    async fn disconnect(&self) -> RakResult<()>;
    async fn recv(&mut self) -> Option<RakEvent>;
    async fn reconnect(&mut self) -> RakResult<()>;
}

/// Production transport: owns a [`RakHandle`] and the [`RakEvent`] receiver, and can rebuild both
/// when the FSM asks to reconnect.
pub(crate) struct RakTransport {
    server: SocketAddr,
    config: RakConfig,
    handle: RakHandle,
    events: mpsc::Receiver<RakEvent>,
}

impl RakTransport {
    pub(crate) async fn connect(server: SocketAddr, config: RakConfig) -> RakResult<Self> {
        let (handle, events) = RakPeer::connect(server, config.clone()).await?;
        Ok(Self {
            server,
            config,
            handle,
            events,
        })
    }
}

impl Transport for RakTransport {
    async fn send(&self, data: Vec<u8>, reliability: Reliability, channel: u8) -> RakResult<()> {
        self.handle.send(data, reliability, channel).await
    }

    async fn rpc(&self, rpc_id: u8, payload: Vec<u8>) -> RakResult<()> {
        self.handle.rpc(rpc_id, payload).await
    }

    async fn disconnect(&self) -> RakResult<()> {
        self.handle.disconnect().await
    }

    async fn recv(&mut self) -> Option<RakEvent> {
        self.events.recv().await
    }

    async fn reconnect(&mut self) -> RakResult<()> {
        let (handle, events) = RakPeer::connect(self.server, self.config.clone()).await?;
        self.handle = handle;
        self.events = events;
        Ok(())
    }
}
