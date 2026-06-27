# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

An async Rust/Tokio reimplementation of a SA-MP 0.3.7 console client, reverse-engineered from
`RakSAMP Lite.exe` / the Arizona client. It speaks the real "RakNet 3.x" UDP wire protocol (byte
cipher + reliable/ordered layer) well enough to connect to live servers, drive the full
`Connecting → Joining → Spawned` sequence, and exchange chat. The end goal is an Android↔PC chat
bridge for Arizona servers.

## Commands

```sh
cargo build --workspace

# the gate (all three must pass before claiming completion):
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace                    # all unit/integration tests
cargo test -p samp-proto                  # one crate
cargo test -p samp-proto client_join_golden_vector   # one test by name
cargo test --workspace -- --include-ignored          # also run live/e2e tests gated by #[ignore]
```

The gate must be green before claiming completion: `cargo fmt --all --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --workspace`.

Run the client (binary is **`rakclient`**, not the crate name `app`):

```sh
cargo run -p app -- --server <host:port> --nick <Nick> [--account-password <pw>]
RUST_LOG=info ./target/release/rakclient.exe --server bumblebee.arizona-rp.com:7777 --nick <Nick> --account-password <pw>
```

`RUST_LOG` targets are the crate paths (`rakclient`, `raknet::transport`, `samp_client::driver`,
`samp_proto`). Use `raknet::transport=trace` to see every datagram as hex.

## Architecture

Layered workspace; each crate is one seam. Dependencies point downward only.

- **`samp-proto`** — pure codecs, zero I/O. `BitStream{Reader,Writer}` (MSB-first bit packing,
  little-endian multibyte), `RpcId`/`SyncPacketId` enums, and `encode_*`/`decode_*` free functions
  for every RPC/packet body (the id byte is prepended by the transport, never by a codec). This crate
  is the public contract everything else compiles against. Wire layouts are byte-exact ports verified
  against the binary — change them only with a golden-vector test.

- **`raknet`** — the SA-MP RakNet 3.x transport. `cipher.rs` + `tables.rs` (port-keyed 256-byte
  substitution cipher), `reliability.rs` (datagram seq, ACK/NAK, ordered channels, split/reassembly),
  `transport.rs` (the `PeerTask` actor: owns the `UdpSocket`, runs the offline handshake, pumps the
  reliability layer through the cipher, `tokio::select!` over recv/command/tick). `wire.rs` holds the
  handshake/RPC framing constants shared with `test-support` so client and mock frame identically.

- **`samp-client`** — the connection state machine. `driver.rs` `Driver<T: Transport, C: Codec>` is
  the FSM, generic over transport and codec seams so it runs against a scripted fake in unit tests and
  the real `raknet`/`samp-proto` in production. `lib.rs` exposes `Client`, `ClientConfig`(+builder),
  `ConnectionState`, `ClientEvent`. The FSM is pumped one event at a time via `Client::next_event()`,
  which internally `select!`s transport events, the on-foot sync interval (while `Spawned`), the
  reconnect timer, and the script `onUpdate` tick.

- **`app`** — the `rakclient` binary: clap/env config → tracing → pump `next_event()` and log.

- **`samp-script`** — the luau (mlua) scripting host mirroring the MoonLoader/SAMP.Lua API. Wraps each
  RPC/packet in a `bitStream` userdata and runs the script's `registerHandler` chokepoints; embeds a
  typed-Luau `samp.events` port plus a reusable `luau/arizona.luau` launcher (server-specific
  emulation lives here, not in the Rust core). Wired into the client via `connect_with_registry`.

- **`test-support`** — dev-only. `MockSampServer` binds a loopback UDP socket and frames its replies
  through the *same* `raknet::wire`/`cipher` primitives as the real client (the e2e test depends on
  byte-identical framing on both sides). Supports fault injection.

### Control flow to keep in mind

`Driver::next_event()` is the single pump point — it loops `step()` (a biased `select!`) until a
user-facing `ClientEvent` is produced. The transport runs as a detached `PeerTask` actor; the driver
talks to it only through the `Transport` trait (`send`/`rpc`/`recv`/`disconnect`/`reconnect`). When
adding behavior, prefer wiring it into the driver FSM or a codec rather than the transport actor.

## Protocol provenance (don't guess these — they're reversed)

- `ClientJoin`: `version = 4057`, `challengeResponse = serverCookie ^ 0xFD9`. The driver sends it
  through the `onSendRPC` chokepoint, so a script's `onSendClientJoin` can rewrite it to a
  server-specific variant (e.g. Arizona's `modded=1` / fixed auth key / duplicated `challengeResponse`,
  in `luau/arizona.luau`). The 7th field is included whenever a script is attached.
- The cipher is asymmetric: the client encrypts outbound datagrams; the server replies in the clear.
- On-foot sync body is exactly 544 bits / 68 bytes; SA-MP text is **cp1251**, not UTF-8 (use
  `samp_proto::decode_cp1251` for chat).

### Arizona specifics

- Servers run an anti-DDoS filter: the transport sends a raw `SAMP …i` query ping before the handshake
  and periodically, to self-whitelist its source IP. Without it the server drops all packets.
- After join, Arizona expects the `220` CEF/validation packet sequence. This now lives in Luau
  (`luau/arizona.luau`, used by the `example_scripts/arizona_launcher_emulation*.luau`), not in Rust.
  The FSM runs its normal join → class → spawn flow; the script times its own validation via the Lua
  scheduler's `wait()` (like the reference addon's `newTask`/`wait`), so it lands within the server's
  validation window without any Rust-side spawn gate.
- Login uses a `ShowDialog` (`Авторизация`) the driver auto-answers via `RPC_DialogResponse`. **Two
  distinct passwords**: `ClientConfig.password` is the RakNet *connection* password (Arizona has none —
  leave `None`); `account_password` is the login-dialog password. The app flags mirror this:
  `--password` vs `--account-password`. Putting the account password in the connection slot causes an
  `ID_INVALID_PASSWORD` rejection.

## Conventions

- Library crates return `Result`/`Option` and propagate with `?` — no `unwrap`/`expect`/`panic!` on
  caller-reachable paths; `app` uses `anyhow`. Model invalid states out of existence (data-carrying
  enums, newtypes like `PlayerId`).
- New wire formats need a golden-vector or round-trip test next to the codec. Live-network tests are
  `#[ignore]`-gated (run with `--include-ignored`).
- Never log credentials (the account password).
