# RakClient — headless async SA-MP 0.3.7 client (Rust / Tokio)

A **headless** SA-MP 0.3.7 client — a pure async network bot with **no GTA San Andreas, no game
engine, and no rendering**. Unlike a normal SA-MP client (GTA:SA + `samp.dll`), it never launches the
game: it speaks the wire protocol directly. Reverse-engineered from `RakSAMP Lite.exe`, it talks to a
real 0.3.7 server and drives the full connection → play sequence:

```
Disconnected → Connecting → RakNet-connected → Joining (ClientJoin)
→ Joined (InitGame) → ClassSelection → ClassSelected → Spawned → InGame (on-foot sync)
```

Faithful enough to talk to a real 0.3.7 server: it implements the SA-MP "RakNet 3.x" reliable/ordered
UDP transport, the per-datagram byte cipher, and a Lua (Luau) scripting layer that mirrors the
MoonLoader / SAMP.Lua API, so stock scripts can hook every RPC and packet.

[Документация на русском →](README.ru.md)

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `samp-proto` | Pure codecs: `BitStream`, packet/RPC ids, typed (de)serializers. No I/O. |
| `raknet` | SA-MP RakNet 3.x transport: byte cipher, reliability layer, async `RakPeer`. |
| `samp-client` | Connection state machine + high-level `Client` over the transport. |
| `samp-script` | Luau scripting host: `bitStream` + `registerHandler` natives and a typed `samp.events` port. |
| `app` | Binary (`rakclient`): config + tracing + run a client to `Spawned`. |
| `test-support` | Dev-only fixtures + loopback mock SA-MP server for integration tests. |

## Run

```sh
cargo run -p app -- --server <host:port> --nick <Nick> [--scripts-dir example_scripts]
```

`RUST_LOG=info` (or `raknet::transport=trace` to see every datagram as hex). The binary is
`rakclient`, not the crate name `app`.

## Develop

```sh
cargo build --workspace

# the gate (must be green before claiming completion):
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Protocol provenance

The wire format was recovered from the original binary (RPC id table, BitStream semantics, the
ClientJoin handshake `version=4057` / `challengeResponse = cookie ^ 0xFD9`, the on-foot sync layout,
and the port-keyed byte cipher with its 256-byte substitution table). Wire layouts are byte-exact
ports verified against the binary — they change only with a golden-vector test.

## License

[WTFPL](LICENSE) — do what the fuck you want to.
