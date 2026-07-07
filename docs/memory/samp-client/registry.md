# samp-client/registry.rs

## Module (packet-handler registry)
<anchor: module>

Packet-handler registry: the registration-pattern seam between the FSM and observers/scripts.

Handlers register per `(direction, id)` or as a catch-all, and each returns a [`Verdict`]. On
every incoming/outgoing RPC the [`crate::driver::Driver`] runs the registry *before* its own
protocol logic: a `Drop` consumes the packet, a `Rewrite` swaps the body (and chains into later
handlers and the FSM), a `Pass` leaves it untouched. The Luau scripting layer is just one
registered catch-all handler.

## register
<anchor: register>

Register a typed handler for an event, keyed by the event type itself. The event's
[`DirectedEvent`] impl supplies both the direction and the RPC id, and the body is decoded to
`T` before the handler runs — so the handler works with typed fields, never the raw
bitstream. Return [`Action::Rewrite`] with a mutated value to re-encode it.

```text
registry.register::<PlayerStreamIn>(|ev| {
    println!("player {} streamed in at {:?}", ev.player_id, ev.position);
    Action::Pass
});
```

---
