//! RakClient binary: connect a SA-MP client to a server and run it to `Spawned`.
//!
//! Configuration comes from CLI flags with environment-variable fallbacks. The client is driven by
//! pumping [`samp_client::Client::next_event`] and logging every event / state transition via
//! `tracing`, continuing past `Spawned` so on-foot sync is visible, until Ctrl-C.
#![forbid(unsafe_code)]

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use clap::Parser;
use samp_client::{
    Client, ClientConfig, ClientEvent, Direction, LocalPlayer, PacketRegistry, ProxyConfig,
};
use samp_proto::{encode_cp1251, ClassId};
use samp_script::ScriptEngine;
use tokio::io::AsyncBufReadExt;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Parser)]
#[command(name = "rakclient", about = "Async SA-MP 0.3.7 client bot")]
struct Cli {
    /// Server address as `host:port`.
    #[arg(long, env = "RAKCLIENT_SERVER")]
    server: String,

    /// In-game nickname.
    #[arg(long, env = "RAKCLIENT_NICK")]
    nick: String,

    /// RakNet server/connection password, if the server has one (not the account password).
    #[arg(long, env = "RAKCLIENT_PASSWORD")]
    password: Option<String>,

    /// Class id to request at spawn selection.
    #[arg(long, env = "RAKCLIENT_CLASS", default_value_t = 0)]
    class: i32,

    /// Announced client version.
    #[arg(
        long = "client-version",
        env = "RAKCLIENT_CLIENT_VERSION",
        default_value = "0.3.7-R3"
    )]
    client_version: String,

    /// Display resolution `WxH` exposed to scripts as `sampResolutionW` / `sampResolutionH`.
    #[arg(long, env = "RAKCLIENT_RESOLUTION", default_value = "1920x1080")]
    resolution: String,

    /// Directory of Lua/Luau scripts to auto-load — every `*.lua`/`*.luau` in it is loaded into one
    /// engine. A missing or empty directory runs a vanilla client with no scripts.
    #[arg(
        long = "scripts-dir",
        env = "RAKCLIENT_SCRIPTS_DIR",
        default_value = "scripts"
    )]
    scripts_dir: PathBuf,

    /// SOCKS5 proxy to tunnel the game UDP through (connect from the proxy's IP). Accepts
    /// `user:pass@host:port`, `socks5://user:pass@host:port`, or `host:port:user:pass`. When unset,
    /// the first non-empty, non-`#` line of `proxy.txt` (in the working dir) is used automatically.
    #[arg(long, env = "RAKCLIENT_PROXY")]
    proxy: Option<String>,

    /// Capture every RakNet datagram (both directions) to a libpcap file for debugging. Bare `--pcap`
    /// writes a fresh per-session `rakclient-<unixsecs>.pcap` (never overwritten between runs); `--pcap
    /// <path>` uses that exact path verbatim. Dissect it with the `dissect` bin (`cargo run -p app --bin
    /// dissect -- <path>`); it also opens directly in Wireshark as UDP.
    #[arg(long, num_args = 0..=1, default_missing_value = "@auto")]
    pcap: Option<PathBuf>,

    /// Self-spawn after N seconds if the server never drives the spawn. `0` (default) = never
    /// self-spawn: stay spectating. On Arizona an unauthorised self-spawn is kicked as suspected
    /// cheating, so leave this off; a spectating bot still receives chat. Enable only for non-Arizona
    /// servers that legitimately never drive the spawn.
    #[arg(
        long = "self-spawn-timeout",
        env = "RAKCLIENT_SELF_SPAWN_TIMEOUT",
        default_value_t = 0
    )]
    self_spawn_timeout: u64,
}

impl Cli {
    fn into_config(self) -> anyhow::Result<ClientConfig> {
        let server = resolve_server(&self.server)?;
        // Explicit --proxy/env wins and is used verbatim. Otherwise claim one from proxy.txt: the
        // claimed line is removed from the file so a concurrent/next agent can't reuse it and a dead
        // one isn't retried (invalid/unresolvable lines are dropped too).
        let proxy = match self.proxy {
            Some(spec) => Some(parse_proxy(&spec)?),
            None => claim_proxy_from_file(Path::new("proxy.txt")),
        };
        // Bare `--pcap` (sentinel `@auto`) → a fresh per-session file so captures are never overwritten
        // between runs; an explicit `--pcap <path>` is used verbatim (the caller owns collisions).
        let pcap = self.pcap.map(|p| {
            if p.as_os_str() == "@auto" {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                PathBuf::from(format!("rakclient-{secs}.pcap"))
            } else {
                p
            }
        });
        Ok(ClientConfig {
            server,
            nick: self.nick,
            password: self.password,
            client_version: self.client_version,
            default_class: ClassId(self.class),
            gpci: None,
            sync_interval: Duration::from_millis(200),
            reconnect_delay: Duration::from_secs(5),
            self_spawn_timeout: (self.self_spawn_timeout > 0)
                .then(|| Duration::from_secs(self.self_spawn_timeout)),
            proxy,
            pcap,
        })
    }

    /// Parse the `WxH` resolution string into `(width, height)`.
    fn resolution(&self) -> anyhow::Result<(u32, u32)> {
        let (w, h) = self
            .resolution
            .split_once('x')
            .ok_or_else(|| anyhow!("resolution must be `WxH`, got `{}`", self.resolution))?;
        Ok((w.trim().parse()?, h.trim().parse()?))
    }
}

/// Parse a proxy spec into a [`ProxyConfig`]. Accepts `host:port:user:pass` (proxy.txt) or
/// `socks5://user:pass@host:port`.
fn parse_proxy(s: &str) -> anyhow::Result<ProxyConfig> {
    let s = s.trim(); // strip stray CR/whitespace from a copied proxy.txt line
    let s = s.strip_prefix("socks5://").unwrap_or(s);
    let (host, port, user, pass) = if let Some((creds, hostport)) = s.rsplit_once('@') {
        // user:pass@host:port (also matches socks5://user:pass@host:port and the raw SOAX line)
        let (user, pass) = creds
            .split_once(':')
            .ok_or_else(|| anyhow!("proxy creds must be `user:pass` in `user:pass@host:port`"))?;
        let (host, port) = hostport
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("proxy must be `user:pass@host:port`"))?;
        (host, port, user, pass)
    } else {
        // host:port:user:pass (proxy.txt) — pass keeps any trailing colons.
        let mut it = s.splitn(4, ':');
        let host = it.next().unwrap_or_default();
        let port = it
            .next()
            .ok_or_else(|| anyhow!("proxy must be host:port:user:pass"))?;
        let user = it
            .next()
            .ok_or_else(|| anyhow!("proxy must be host:port:user:pass"))?;
        let pass = it
            .next()
            .ok_or_else(|| anyhow!("proxy must be host:port:user:pass"))?;
        (host, port, user, pass)
    };
    let addr = format!("{host}:{port}")
        .to_socket_addrs()
        .with_context(|| format!("resolving proxy address `{host}:{port}`"))?
        .next()
        .ok_or_else(|| anyhow!("no addresses resolved for proxy `{host}:{port}`"))?;
    Ok(ProxyConfig {
        addr,
        username: user.to_string(),
        password: pass.to_string(),
    })
}

/// Claim (pop) the first usable proxy line from `path`, removing it from the file so a concurrent or
/// subsequent agent can't reuse it and a dead line isn't retried. Comment (`#`) and blank lines are
/// preserved. Malformed/unresolvable lines are dropped and the next candidate is tried; returns the
/// parsed config, or `None` if the pool holds no usable proxy. Credentials are never logged.
fn claim_proxy_from_file(path: &Path) -> Option<ProxyConfig> {
    loop {
        let content = std::fs::read_to_string(path).ok()?;
        let lines: Vec<&str> = content.lines().collect();
        let idx = lines.iter().position(|line| {
            let t = line.trim();
            !t.is_empty() && !t.starts_with('#')
        })?;
        let spec = lines[idx].trim().to_string();

        // Rewrite the file without the claimed line (keep a trailing newline if anything remains).
        let mut kept = lines.clone();
        kept.remove(idx);
        let mut rest = kept.join("\n");
        if !rest.is_empty() {
            rest.push('\n');
        }
        let removed = std::fs::write(path, &rest).is_ok();
        if !removed {
            warn!("could not rewrite proxy.txt; using the proxy without removing it from the pool");
        }

        match parse_proxy(&spec) {
            Ok(cfg) => {
                info!(addr = %cfg.addr, "claimed proxy from proxy.txt (removed from pool)");
                return Some(cfg);
            }
            Err(error) => {
                warn!(%error, "dropped an invalid proxy line from proxy.txt");
                // If we couldn't remove it, stop to avoid re-reading the same bad line forever.
                if !removed {
                    return None;
                }
            }
        }
    }
}

fn resolve_server(server: &str) -> anyhow::Result<SocketAddr> {
    server
        .to_socket_addrs()
        .with_context(|| format!("resolving server address `{server}`"))?
        .next()
        .ok_or_else(|| anyhow!("no addresses resolved for `{server}`"))
}

fn init_tracing() {
    use tracing_subscriber::prelude::*;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stdout_layer = tracing_subscriber::fmt::layer();

    // Also mirror every log line to a file (path from RAKCLIENT_LOG, default `rakclient.log`),
    // truncated per run, so a session can be inspected after the fact without scraping the console.
    // Best-effort: if the file can't be opened, fall back to stdout only.
    let log_path = std::env::var("RAKCLIENT_LOG").unwrap_or_else(|_| "rakclient.log".to_string());
    match std::fs::File::create(&log_path) {
        Ok(file) => {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file));
            tracing_subscriber::registry()
                .with(filter)
                .with(stdout_layer)
                .with(file_layer)
                .init();
            tracing::info!(path = %log_path, "logging to file");
        }
        Err(e) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(stdout_layer)
                .init();
            tracing::warn!(path = %log_path, error = %e, "could not open log file; stdout only");
        }
    }
}

/// Collect loadable Lua/Luau scripts from `dir`, sorted by name for a deterministic load order. A
/// missing directory yields an empty list (not an error), so running without scripts is valid.
fn collect_scripts(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading scripts dir `{}`", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("lua") | Some("luau")
                )
        })
        .collect();
    files.sort();
    Ok(files)
}

fn dispatch_to_script(engine: &ScriptEngine, event: &ClientEvent) {
    match event {
        ClientEvent::Chat { player_id, text } => engine.on_chat(player_id.0, text),
        ClientEvent::ServerMessage { color, text } => engine.on_server_message(*color, text),
        // Connection-lifecycle events → append-based chokepoints, so job scripts can auto-start on
        // spawn (there is no local chat/command input in the headless client to trigger them).
        ClientEvent::Connected => engine.dispatch_lifecycle("onConnect"),
        ClientEvent::Spawned => engine.dispatch_lifecycle("onSpawn"),
        ClientEvent::Disconnected(_) => engine.dispatch_lifecycle("onDisconnect"),
        _ => {}
    }
}

/// Send each already-cp1251-encoded chat line as the local player. The single funnel for every
/// outgoing chat source (stdin probe and script `drain_outgoing_chat`).
async fn send_chat_lines(client: &mut Client, lines: impl IntoIterator<Item = Vec<u8>>) {
    for line in lines {
        client.send_chat(&line).await;
    }
}

fn log_event(event: &ClientEvent) {
    match event {
        ClientEvent::StateChanged(state) => info!(?state, "state transition"),
        ClientEvent::Connected => info!("raknet transport connected"),
        ClientEvent::Joined {
            local_id,
            host_name,
        } => info!(?local_id, host_name = %host_name, "joined game"),
        ClientEvent::Spawned => info!("spawned — continuing to show on-foot sync"),
        ClientEvent::ServerMessage { color, text } => {
            info!(color = format_args!("{color:08X}"), "server: {text}")
        }
        ClientEvent::Chat { player_id, text } => info!(?player_id, "chat: {text}"),
        ClientEvent::Disconnected(reason) => warn!(reason = %reason, "disconnected"),
    }
}

async fn wire_script_engine(
    engine: &Rc<ScriptEngine>,
    config: ClientConfig,
    scripts: &[PathBuf],
    resolution: (u32, u32),
) -> anyhow::Result<Client> {
    let mut registry = PacketRegistry::new();
    // Shared bot state for getBot*/setBot*, mirrored by the driver.
    let bot_state = LocalPlayer::shared(config.nick.clone(), config.server);
    engine
        .install_bindings(bot_state.clone())
        .map_err(|e| anyhow!("installing bot bindings: {e}"))?;
    // Wire script-initiated sends and hand the script its connection context.
    engine
        .install_sender(registry.outbox())
        .map_err(|e| anyhow!("installing script sender: {e}"))?;
    let (width, height) = resolution;
    for (key, value) in [("sampResolutionW", width), ("sampResolutionH", height)] {
        engine
            .set_global(key, value)
            .map_err(|e| anyhow!("setting {key}: {e}"))?;
    }
    engine
        .set_global("sampNick", config.nick.clone())
        .map_err(|e| anyhow!("setting sampNick: {e}"))?;
    // Load each script now that bindings/globals are in place (a script may use them on load).
    for path in scripts {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("reading script `{}`", path.display()))?;
        engine
            .load_script(&source, &path.display().to_string())
            .map_err(|e| anyhow!("loading script `{}`: {e}", path.display()))?;
        info!(script = %path.display(), "lua script loaded");
    }
    // Lifecycle hooks: the script sends its own connect/init packets in FSM sequence.
    let on_connect = engine.clone();
    registry.on_lifecycle("onConnect", move || on_connect.fire("onConnect", ()));
    let on_init = engine.clone();
    registry.on_lifecycle("onInitGame", move || on_init.fire("onInitGame", ()));
    // Route every incoming/outgoing RPC and packet through its `registerHandler` chokepoint.
    let rpc_handler = engine.clone();
    registry.on_any_rpc(move |direction, id, payload| {
        let name = match direction {
            Direction::Incoming => "onReceiveRPC",
            Direction::Outgoing => "onSendRPC",
        };
        rpc_handler.dispatch_chokepoint(name, id, payload)
    });
    let packet_handler = engine.clone();
    registry.on_any_packet(move |direction, id, payload| {
        let name = match direction {
            Direction::Incoming => "onReceivePacket",
            Direction::Outgoing => "onSendPacket",
        };
        packet_handler.dispatch_chokepoint(name, id, payload)
    });
    // The task scheduler tick (`newTask`/`wait` run on `registerHandler('onUpdate')`).
    let update_handler = engine.clone();
    registry.on_update(move || update_handler.dispatch_update());
    Client::connect_with_registry(config, registry, bot_state)
        .await
        .context("failed to connect to server")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let resolution = cli.resolution()?;
    let scripts = collect_scripts(&cli.scripts_dir)?;
    let config = cli.into_config()?;
    info!(server = %config.server, nick = %config.nick, scripts = scripts.len(), proxy = config.proxy.is_some(), "connecting");

    // Build one engine for every script in the scripts dir; with none, run a vanilla client.
    let engine: Option<Rc<ScriptEngine>> = if scripts.is_empty() {
        None
    } else {
        Some(Rc::new(
            ScriptEngine::new().map_err(|e| anyhow!("initialising lua: {e}"))?,
        ))
    };

    let mut client = match &engine {
        Some(engine) => wire_script_engine(engine, config, &scripts, resolution).await?,
        None => Client::connect(config)
            .await
            .context("failed to connect to server")?,
    };
    info!(state = ?client.state(), "connection task started");

    // Interactive chat probe: each line typed on stdin is sent as a chat line (transcoded to
    // cp1251), so a live session can be tested for connectivity/visibility — e.g. detecting a
    // silent ban — by typing and watching whether it lands.
    //
    // Read stdin on a dedicated task that forwards lines over a channel, rather than polling
    // `stdin.next_line()` inside the main select. The blocking console read can't be cancelled, so
    // sitting it in the select and dropping it every loop iteration swallowed the first Ctrl-C
    // (needing a second press). Off in its own task it never contends with signal handling.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = stdin.next_line().await {
            if line_tx.send(line).is_err() {
                break; // main loop gone
            }
        }
    });
    let mut stdin_open = true;

    loop {
        tokio::select! {
            // Biased: Ctrl-C is checked before stdin/events so a shutdown always wins the first press.
            biased;
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received, shutting down");
                break;
            }
            maybe_line = line_rx.recv(), if stdin_open => {
                match maybe_line {
                    Some(text) => {
                        let text = text.trim_end_matches(['\r', '\n']);
                        if !text.is_empty() {
                            // No local echo: the server broadcasts our chat back and that line is
                            // already logged. Silence here means the message never landed (mute/ban).
                            send_chat_lines(&mut client, [encode_cp1251(text)]).await;
                        }
                    }
                    None => {
                        info!("stdin closed; chat input disabled");
                        stdin_open = false;
                    }
                }
            }
            event = client.next_event() => {
                match event {
                    Some(event) => {
                        log_event(&event);
                        if let Some(engine) = &engine {
                            dispatch_to_script(engine, &event);
                            send_chat_lines(&mut client, engine.drain_outgoing_chat()).await;
                        }
                    }
                    None => {
                        info!("client closed");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_args_with_defaults() {
        let cli = Cli::try_parse_from(["rakclient", "--server", "127.0.0.1:7777", "--nick", "Bot"])
            .expect("valid args");
        assert_eq!(cli.server, "127.0.0.1:7777");
        assert_eq!(cli.nick, "Bot");
        assert_eq!(cli.password, None);
        assert_eq!(cli.class, 0);
        assert_eq!(cli.client_version, "0.3.7-R3");
        assert_eq!(cli.resolution, "1920x1080");
        assert_eq!(cli.scripts_dir, PathBuf::from("scripts"));
    }

    #[test]
    fn parses_overrides() {
        let cli = Cli::try_parse_from([
            "rakclient",
            "--server",
            "127.0.0.1:7777",
            "--nick",
            "Bot",
            "--password",
            "secret",
            "--class",
            "5",
            "--client-version",
            "0.3.DL",
        ])
        .expect("valid args");
        assert_eq!(cli.password.as_deref(), Some("secret"));
        assert_eq!(cli.class, 5);
        assert_eq!(cli.client_version, "0.3.DL");
    }

    #[test]
    fn missing_required_args_is_an_error() {
        assert!(Cli::try_parse_from(["rakclient"]).is_err());
    }

    #[test]
    fn config_resolves_server_and_class() {
        let cli = Cli::try_parse_from([
            "rakclient",
            "--server",
            "127.0.0.1:7777",
            "--nick",
            "Bot",
            "--class",
            "3",
        ])
        .expect("valid args");
        let config = cli.into_config().expect("config builds");
        assert_eq!(
            config.server,
            "127.0.0.1:7777".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.default_class, ClassId(3));
    }

    #[test]
    fn parses_proxy_forms() {
        // Raw SOAX line: user:pass@host:port (numeric host so the test doesn't hit DNS).
        let p = parse_proxy("country-ru-session-abc:Pass123@127.0.0.1:1337").unwrap();
        assert_eq!(p.username, "country-ru-session-abc");
        assert_eq!(p.password, "Pass123");
        assert_eq!(p.addr.to_string(), "127.0.0.1:1337");
        // socks5:// prefix and the legacy host:port:user:pass form still parse.
        let p = parse_proxy("socks5://u:p@127.0.0.1:1080").unwrap();
        assert_eq!((p.username.as_str(), p.password.as_str()), ("u", "p"));
        let p = parse_proxy("127.0.0.1:1080:user:pass").unwrap();
        assert_eq!((p.username.as_str(), p.password.as_str()), ("user", "pass"));
    }

    #[test]
    fn claim_proxy_pops_lines_and_keeps_comments() {
        let path = std::env::temp_dir().join(format!("rakclient_proxy_{}.txt", std::process::id()));
        std::fs::write(
            &path,
            "# pool\nu1:p1@127.0.0.1:1080\nu2:p2@127.0.0.1:1081\n",
        )
        .unwrap();

        // Each claim pops the next usable line; the comment survives; then the pool is empty.
        assert_eq!(
            claim_proxy_from_file(&path).unwrap().addr.to_string(),
            "127.0.0.1:1080"
        );
        assert_eq!(
            claim_proxy_from_file(&path).unwrap().addr.to_string(),
            "127.0.0.1:1081"
        );
        assert!(claim_proxy_from_file(&path).is_none());
        assert!(std::fs::read_to_string(&path).unwrap().contains("# pool"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_unresolvable_server() {
        let cli = Cli::try_parse_from(["rakclient", "--server", "not a host", "--nick", "Bot"])
            .expect("args parse");
        assert!(cli.into_config().is_err());
    }
}
