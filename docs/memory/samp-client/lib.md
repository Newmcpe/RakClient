# samp-client/lib.rs

## Module (connection state machine + Client)
<anchor: module>

SA-MP connection state machine and high-level [`Client`].

Drives the reversed connect → play sequence over the [`raknet`] transport using [`samp_proto`]
codecs:

```text
Disconnected → Connecting → RakNetConnected → Joining → Joined
  → ClassSelection → ClassSelected → Spawned
```

The crate root holds the public contract; the FSM lives in the private [`driver`] module driven
over the [`transport`] seam.

## ClientConfig::self_spawn_timeout
<anchor: self-spawn-timeout>

If set, and the server never drives the spawn (`SetSpawnInfo`/`TogglePlayerSpectating`),
self-spawn after this long in the pre-spawn window. `None` ⇒ never self-spawn: stay
spectating (the correct mode for Arizona, whose anti-cheat kicks an unauthorised RPC_Spawn as
"подозрение в читерстве"; a spectating client still receives chat/world state).

## connect_with_registry
<anchor: connect-with-registry>

Connect with a [`PacketRegistry`] attached: registered handlers (scripts/observers) intercept
every incoming/outgoing RPC before the FSM, and `on_update` handlers fire on the driver's
update tick. The registry holds non-`Send` script closures, so a client built this way is
itself `!Send` — drive it inline (do not `tokio::spawn` it).

## e2e test: end_to_end_reaches_spawned
<anchor: e2e-reaches-spawned>

Full stack over loopback UDP: the real [`raknet`] transport drives the connect → spawn
handshake against [`MockSampServer`], which frames its replies through the same
[`raknet::wire`] primitives. Each phase is wrapped in a [`tokio::time::timeout`] so a future
wire-framing regression fails fast instead of hanging.

Inline note on the got_sync wait: Keep pumping the FSM so the on-foot sync timer fires, and wait
for the mock to record at least one sync packet. `next_event` ticks syncs internally without
yielding an event, and a `Client` is `!Send` (it may carry script handlers), so the pump runs
concurrently on this task via `select!` rather than a spawned task — the wait branch wins once a
sync lands.

---
