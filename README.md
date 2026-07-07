# RakClient: a headless async SA-MP 0.3.7 client (Rust / Tokio)

RakClient is a self-sufficient SA-MP 0.3.7 client for interacting with servers without launching the
original game, built first and foremost for **Arizona RP**. It plays the whole connection-to-spawn
sequence and then runs whatever you script for it — jobs, automation, an Android↔PC chat bridge, or
anything else you write in Lua.

It grows out of [RakSAMP Lite](https://www.blast.hk/threads/108052/) — the lightweight client that inspired it, whose
protocol it covers in full — and goes further: a real navmesh, a native walker, and a first-class Luau
host tuned for Arizona's servers.

You write behaviour as Lua (Luau) scripts against a host API that mirrors MoonLoader / SAMP.Lua, so a
script hooks every RPC and packet the same way it would inside the real game. Under that sits the SA-MP
"RakNet 3.x" reliable/ordered UDP transport, the per-datagram byte cipher, and the connection state
machine.

[Документация на русском](README.ru.md)

## Walking like a real player

The bot moves over a real navigation mesh built from the server's own map, and it walks with the GTA-SA
gaits: walk, jog, and sprint, at speeds and animations measured from a live capture of the game client
(`walkTo(x, y, z, "walk"|"jog"|"sprint")`). A script asks it to go somewhere and it finds a path around
the props and buildings, ramping up to speed and reporting a matching analog input, velocity, and
animation so it reads as a genuine player to everyone else on the server.

Below is the offline viewer showing Arizona RP's sawmill, reconstructed from the game files, with a
navmesh route (yellow) planned across the yard. The green carpet is the walkable surface.

![Sawmill navmesh, top-down](docs/img/sawmill-navmesh-top.png)

![Sawmill navmesh, path through the forest yard](docs/img/sawmill-navmesh-forest.png)

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `samp-proto` | Pure codecs: `BitStream`, packet/RPC ids, typed (de)serializers. No I/O. |
| `raknet` | RakNet 3.x transport: byte cipher, reliability layer, async `RakPeer`, plus an offline pcap dissector. |
| `samp-client` | Connection state machine, the high-level `Client`, and the native `walkTo` walker. |
| `samp-script` | Luau scripting host: `bitStream` + `registerHandler` natives and a typed `samp.events` port. |
| `sa-map` | Parser for GTA/Arizona world data (IMG, IDE, IPL, COL, DFF, the streamer bin) and a baked-scene format. |
| `sa-nav` | Navmesh generation (`navgen`) over `navmesh-recast`, the `.nav` format, and runtime pathfinding. |
| `navmesh-recast` | The Recast build pipeline (a fork of `rerecast`); machine-local until published. |
| `sa-viewer` | A Bevy flythrough of the parsed world and the navmesh. Its own workspace, to keep Bevy out of the main gate. |
| `app` | Binary (`rakclient`): config, tracing, and the `dissect` / `objects` / `rpcscan` capture tools. |
| `test-support` | Dev-only fixtures and a loopback mock SA-MP server for integration tests. |

## Run

```sh
cargo run -p app -- --server <host:port> --nick <Nick> [--scripts-dir example_scripts]
```

The binary is `rakclient`, not the crate name `app`. Set `RUST_LOG=info` for logs, or
`raknet::transport=trace` to see every datagram as hex. Pass `--navmesh <file.nav>` to enable the native
`walkTo` walker in scripts.

## World and navigation tools

The bot lives on Arizona's custom map, so a few tools reconstruct and navigate it offline.

Bake the world once, then view it:

```sh
cargo run -p sa-map --bin samap -- scene <gta3.img> <data-dir> world.scene [objects.csv]
cargo run --release --manifest-path crates/sa-viewer/Cargo.toml -- world.scene [nav.nav]
```

Build a navmesh for a region (the bot walks over it via `walkTo`):

```sh
cargo run -p sa-nav --features build --bin navgen -- <gta3.img> <data-dir> region.nav [objects.csv]
```

Capture and inspect traffic. `rakclient --pcap` writes a libpcap file; the MoonLoader script at
`tools/moonloader/rpc_capture_pcap.lua` writes the same format from the real client. Both open in the
offline tools:

```sh
cargo run -p app --bin dissect -- capture.pcap   # per-datagram RPC/packet decode
cargo run -p app --bin objects -- capture.pcap   # extract streamed CreateObject placements
cargo run -p app --bin rpcscan -- capture.pcap   # RPC/packet census + needle search
```

## Develop

```sh
cargo build --workspace

# the gate (must be green before claiming completion):
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
```

## License

[WTFPL](LICENSE), do what the fuck you want to.
