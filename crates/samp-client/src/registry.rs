//! Packet-handler registry: the registration-pattern seam between the FSM and observers/scripts.
//!
//! Handlers register per `(direction, id)` or as a catch-all, and each returns a [`Verdict`]. On
//! every incoming/outgoing RPC the [`crate::driver::Driver`] runs the registry *before* its own
//! protocol logic: a `Drop` consumes the packet, a `Rewrite` swaps the body (and chains into later
//! handlers and the FSM), a `Pass` leaves it untouched. The Luau scripting layer is just one
//! registered catch-all handler.

use std::collections::HashMap;

use samp_proto::events::{decode_event, encode_event, DirectedEvent};
use samp_proto::{Direction, OutboundMsg, Outbox, Verdict};

/// A typed handler's decision about a decoded event. Like [`Verdict`] but the rewrite carries the
/// (possibly mutated) typed event, which the registry re-encodes.
pub enum Action<T> {
    /// Forward the event unchanged.
    Pass,
    /// Consume the event.
    Drop,
    /// Replace the event with this value (re-encoded to bytes).
    Rewrite(T),
}

/// Sees one RPC body and decides its fate.
type RpcHandler = Box<dyn Fn(Direction, u8, &[u8]) -> Verdict>;
/// Periodic tick (drives script timers/coroutines).
type UpdateHandler = Box<dyn Fn()>;

/// A collection of packet handlers keyed by direction + id, plus catch-all and periodic hooks. Not
/// `Send`/`Sync`: it lives on the client's thread alongside the (non-`Send`) script VM.
#[derive(Default)]
pub struct PacketRegistry {
    by_id: HashMap<(Direction, u8), Vec<RpcHandler>>,
    any: Vec<RpcHandler>,
    any_packet: Vec<RpcHandler>,
    update: Vec<UpdateHandler>,
    lifecycle: HashMap<&'static str, Vec<UpdateHandler>>,
    outbox: Outbox,
}

impl PacketRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed handler for an event, keyed by the event type itself. The event's
    /// [`DirectedEvent`] impl supplies both the direction and the RPC id, and the body is decoded to
    /// `T` before the handler runs — so the handler works with typed fields, never the raw
    /// bitstream. Return [`Action::Rewrite`] with a mutated value to re-encode it.
    ///
    /// ```text
    /// registry.register::<PlayerStreamIn>(|ev| {
    ///     println!("player {} streamed in at {:?}", ev.player_id, ev.position);
    ///     Action::Pass
    /// });
    /// ```
    pub fn register<T>(&mut self, handler: impl Fn(T) -> Action<T> + 'static) -> &mut Self
    where
        T: DirectedEvent + 'static,
    {
        self.on_rpc(T::DIRECTION, T::RPC_ID, move |_, _, payload| {
            let Ok(event) = decode_event::<T>(payload) else {
                return Verdict::Pass; // malformed body: don't interfere
            };
            match handler(event) {
                Action::Pass => Verdict::Pass,
                Action::Drop => Verdict::Drop,
                Action::Rewrite(event) => Verdict::Rewrite(encode_event(&event)),
            }
        })
    }

    /// Register a handler for one `(direction, id)`. Multiple handlers for the same key run in
    /// registration order.
    pub fn on_rpc(
        &mut self,
        direction: Direction,
        id: u8,
        handler: impl Fn(Direction, u8, &[u8]) -> Verdict + 'static,
    ) -> &mut Self {
        self.by_id
            .entry((direction, id))
            .or_default()
            .push(Box::new(handler));
        self
    }

    /// Register a handler that sees every RPC in `direction` (it dispatches by id itself — this is
    /// how the script engine plugs in).
    pub fn on_any_rpc(
        &mut self,
        handler: impl Fn(Direction, u8, &[u8]) -> Verdict + 'static,
    ) -> &mut Self {
        self.any.push(Box::new(handler));
        self
    }

    /// Register a handler that sees every raw packet (incoming and outgoing) — `onReceivePacket` /
    /// `onSendPacket`. Like [`Self::on_any_rpc`] but for the packet chokepoints.
    pub fn on_any_packet(
        &mut self,
        handler: impl Fn(Direction, u8, &[u8]) -> Verdict + 'static,
    ) -> &mut Self {
        self.any_packet.push(Box::new(handler));
        self
    }

    /// Register a periodic tick handler (fired on the driver's update interval).
    pub fn on_update(&mut self, handler: impl Fn() + 'static) -> &mut Self {
        self.update.push(Box::new(handler));
        self
    }

    /// Whether any periodic handler is registered (the driver only arms its update timer if so).
    pub fn wants_update(&self) -> bool {
        !self.update.is_empty() || !self.lifecycle.is_empty()
    }

    /// Register a connection-lifecycle handler (e.g. `"onConnect"`, `"onInitGame"`), fired by the
    /// driver at the corresponding FSM point so scripts can send packets in sequence.
    pub fn on_lifecycle(&mut self, event: &'static str, handler: impl Fn() + 'static) -> &mut Self {
        self.lifecycle
            .entry(event)
            .or_default()
            .push(Box::new(handler));
        self
    }

    /// The shared outbox scripts push into. The host (script VM) takes a clone of this to wire its
    /// `sampSendPacket`/`sampSendRpc` bindings.
    pub fn outbox(&self) -> Outbox {
        self.outbox.clone()
    }

    /// Fire the lifecycle handlers registered for `event`.
    pub(crate) fn dispatch_lifecycle(&self, event: &str) {
        if let Some(handlers) = self.lifecycle.get(event) {
            for handler in handlers {
                handler();
            }
        }
    }

    /// Take everything scripts have queued to send since the last drain.
    pub(crate) fn drain_outbox(&self) -> Vec<OutboundMsg> {
        self.outbox.borrow_mut().drain(..).collect()
    }

    /// Run the catch-all handlers then the id-specific handlers for `(direction, id)`. The first
    /// `Drop` wins immediately; `Rewrite`s chain (each later handler — and the FSM — sees the latest
    /// body). Returns the combined verdict.
    pub(crate) fn dispatch_rpc(&self, direction: Direction, id: u8, payload: &[u8]) -> Verdict {
        let id_handlers = self.by_id.get(&(direction, id)).into_iter().flatten();
        run_chain(self.any.iter().chain(id_handlers), direction, id, payload)
    }

    /// Run the packet chokepoint handlers (`onReceivePacket`/`onSendPacket`).
    pub(crate) fn dispatch_packet(&self, direction: Direction, id: u8, payload: &[u8]) -> Verdict {
        run_chain(self.any_packet.iter(), direction, id, payload)
    }

    /// Fire every periodic handler.
    pub(crate) fn tick(&self) {
        for handler in &self.update {
            handler();
        }
    }
}

/// Run a chain of handlers over a body: first `Drop` wins; `Rewrite`s chain (each later handler
/// sees the latest body); otherwise the (possibly rewritten) result is returned.
fn run_chain<'a>(
    handlers: impl Iterator<Item = &'a RpcHandler>,
    direction: Direction,
    id: u8,
    payload: &[u8],
) -> Verdict {
    let mut rewritten: Option<Vec<u8>> = None;
    for handler in handlers {
        let body = rewritten.as_deref().unwrap_or(payload);
        match handler(direction, id, body) {
            Verdict::Pass => {}
            Verdict::Drop => return Verdict::Drop,
            Verdict::Rewrite(bytes) => rewritten = Some(bytes),
        }
    }
    match rewritten {
        Some(bytes) => Verdict::Rewrite(bytes),
        None => Verdict::Pass,
    }
}

#[cfg(test)]
mod tests;
