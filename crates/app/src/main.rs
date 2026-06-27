//! RakClient binary: connect a SA-MP client to a server and run it to `Spawned`.
//!
//! Configuration comes from CLI flags with environment-variable fallbacks. The client is driven by
//! pumping [`samp_client::Client::next_event`] and logging every event / state transition via
//! `tracing`, continuing past `Spawned` so on-foot sync is visible, until Ctrl-C.
#![forbid(unsafe_code)]

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use clap::Parser;
use samp_client::{BotState, Client, ClientConfig, ClientEvent, Direction, PacketRegistry};
use samp_proto::ClassId;
use samp_script::ScriptEngine;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// CLI / environment configuration for the RakClient client.
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

    /// Account login password, typed into the server's login dialog.
    #[arg(long = "account-password", env = "RAKCLIENT_ACCOUNT_PASSWORD")]
    account_password: Option<String>,

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

    /// Disable the Arizona ClientJoin variant (send a strict-vanilla join).
    #[arg(long = "no-arizona-compat", env = "RAKCLIENT_NO_ARIZONA_COMPAT")]
    no_arizona_compat: bool,

    /// Disable native aim-sync (the believable rate-limited camera/aim sent while spawned).
    #[arg(long = "no-aim-sync", env = "RAKCLIENT_NO_AIM_SYNC")]
    no_aim_sync: bool,

    /// Path to a Lua script to load. The script's `onChatMessage`/`onServerMessage` callbacks are
    /// invoked as those events arrive.
    #[arg(long, env = "RAKCLIENT_SCRIPT")]
    script: Option<PathBuf>,
}

impl Cli {
    fn into_config(self) -> anyhow::Result<ClientConfig> {
        let server = resolve_server(&self.server)?;
        Ok(ClientConfig {
            server,
            nick: self.nick,
            password: self.password,
            account_password: self.account_password,
            client_version: self.client_version,
            default_class: ClassId(self.class),
            gpci: None,
            sync_interval: Duration::from_millis(200),
            reconnect_delay: Duration::from_secs(5),
            arizona_compat: !self.no_arizona_compat,
            az_app_version: "onSvelteAppVersion|1.0.0|ea94b29b".to_string(),
            az_resolution: (1920, 1080),
            aim_sync: !self.no_aim_sync,
        })
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
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Load and initialise the Lua engine from `path`, or return `None` when no script was requested.
fn load_script_engine(path: Option<PathBuf>) -> anyhow::Result<Option<ScriptEngine>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("reading script `{}`", path.display()))?;
    let engine = ScriptEngine::new().map_err(|e| anyhow!("initialising lua: {e}"))?;
    engine
        .load_script(&source, &path.display().to_string())
        .map_err(|e| anyhow!("loading script `{}`: {e}", path.display()))?;
    info!(script = %path.display(), "lua script loaded");
    Ok(Some(engine))
}

/// Forward decoded events into the script's matching callbacks.
fn dispatch_to_script(engine: &ScriptEngine, event: &ClientEvent) {
    match event {
        ClientEvent::Chat { player_id, text } => engine.on_chat(player_id.0, text),
        ClientEvent::ServerMessage { color, text } => engine.on_server_message(*color, text),
        _ => {}
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let engine = load_script_engine(cli.script.clone())?.map(Rc::new);
    let config = cli.into_config()?;
    info!(server = %config.server, nick = %config.nick, "connecting");

    // With a script loaded, register it as a catch-all RPC handler so every incoming/outgoing RPC
    // reaches its `sampev.onX` callback (and can be dropped/rewritten) before the FSM.
    let mut client = match &engine {
        Some(engine) => {
            let mut registry = PacketRegistry::new();
            // Shared bot state for getBot*/setBot*, mirrored by the driver.
            let bot_state = BotState::shared(config.nick.clone(), config.server);
            engine
                .install_bindings(bot_state.clone())
                .map_err(|e| anyhow!("installing bot bindings: {e}"))?;
            // Wire script-initiated sends and hand the script its connection context.
            engine
                .install_sender(registry.outbox())
                .map_err(|e| anyhow!("installing script sender: {e}"))?;
            let (width, height) = config.az_resolution;
            for (key, value) in [("sampResolutionW", width), ("sampResolutionH", height)] {
                engine
                    .set_global(key, value)
                    .map_err(|e| anyhow!("setting {key}: {e}"))?;
            }
            engine
                .set_global("sampNick", config.nick.clone())
                .map_err(|e| anyhow!("setting sampNick: {e}"))?;
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
                .context("failed to connect to server")?
        }
        None => Client::connect(config)
            .await
            .context("failed to connect to server")?,
    };
    info!(state = ?client.state(), "connection task started");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received, shutting down");
                break;
            }
            event = client.next_event() => {
                match event {
                    Some(event) => {
                        log_event(&event);
                        if let Some(engine) = &engine {
                            dispatch_to_script(engine, &event);
                            for line in engine.drain_outgoing_chat() {
                                client.send_chat(&line).await;
                            }
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
        assert!(!cli.no_arizona_compat); // Arizona ClientJoin variant on by default
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
    fn rejects_unresolvable_server() {
        let cli = Cli::try_parse_from(["rakclient", "--server", "not a host", "--nick", "Bot"])
            .expect("args parse");
        assert!(cli.into_config().is_err());
    }
}
