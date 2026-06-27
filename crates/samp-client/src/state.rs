//! Shared local-bot state, the bridge between the driver (authoritative on-foot sync) and the
//! script engine's `getBot*`/`setBot*` bindings.
//!
//! The driver mirrors its [`samp_proto::OnFootSync`] fields into here and reads them back when
//! building the next sync packet; the engine reads/writes the same `Rc<RefCell<_>>`. Position,
//! rotation, health, armour, and weapon are authoritative today; money/interior/vehicle/camera are
//! placeholders until the relevant RPCs are decoded into them (Phase 6).

use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;

use samp_proto::{Quaternion, Vector3};

/// The local bot's mutable state, shared between driver and scripts.
#[derive(Debug, Clone)]
pub struct BotState {
    pub nick: String,
    pub server_addr: SocketAddr,
    pub position: Vector3,
    pub rotation: Quaternion,
    pub health: u8,
    pub armour: u8,
    pub weapon: u8,
    /// `0` = on foot. Tracked from vehicle RPCs in Phase 6.
    pub vehicle: u16,
    pub money: i32,
    pub interior: u8,
    pub camera_pos: Vector3,
    /// Set by `updateSync()`; the driver sends a sync immediately and clears it.
    pub force_sync: bool,
    /// Set by `reconnect(ms)`; the driver schedules a reconnect after this delay and clears it.
    pub reconnect_in_ms: Option<u64>,
}

/// `BotState` shared between the (non-`Send`) driver and script engine on the client thread.
pub type SharedBotState = Rc<RefCell<BotState>>;

impl BotState {
    pub fn new(nick: String, server_addr: SocketAddr) -> Self {
        Self {
            nick,
            server_addr,
            position: Vector3::default(),
            rotation: Quaternion::default(),
            health: 0,
            armour: 0,
            weapon: 0,
            vehicle: 0,
            money: 0,
            interior: 0,
            camera_pos: Vector3::default(),
            force_sync: false,
            reconnect_in_ms: None,
        }
    }

    /// Build a shared handle seeded from the connection config.
    pub fn shared(nick: String, server_addr: SocketAddr) -> SharedBotState {
        Rc::new(RefCell::new(Self::new(nick, server_addr)))
    }
}
