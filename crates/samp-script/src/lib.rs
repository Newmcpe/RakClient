//! Lua (luau via mlua) scripting host mirroring the MoonLoader / SAMP.Lua API.
//!
//! The host wraps each SA-MP RPC/packet in a `bitStream` userdata and runs the script's
//! `registerHandler` chokepoints (`onReceiveRPC`/`onSendRPC`/`onReceivePacket`/`onSendPacket`/
//! `onUpdate`); a handler returning `false` consumes the packet and a mutated stream rewrites it.
//! The VM is `!Send` and lives on the client's thread; dispatch is synchronous.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Function, IntoLua, IntoLuaMulti, Lua, Table, Value, Variadic};

mod bindings;
mod bitstream;
mod crypto;
mod require;
use samp_proto::{Encode, OutboundMsg, Outbox, Verdict};

/// Outgoing chat lines a script queued via `sampSendChat`, buffered until the host drains them.
/// Already encoded to the wire encoding (cp1251): scripts pass UTF-8 and the host transcodes.
type OutgoingChat = Rc<RefCell<Vec<Vec<u8>>>>;

/// Owns a luau VM and the scripts loaded into it.
pub struct ScriptEngine {
    lua: Lua,
    outgoing_chat: OutgoingChat,
    /// The driver's send queue, used to build `bitStream` userdata for `registerHandler`
    /// chokepoints so handlers can `sendPacket`. Set by [`ScriptEngine::install_sender`].
    outbox: RefCell<Outbox>,
}

impl ScriptEngine {
    /// Build a VM with the host bindings (`print`, `sampSendChat`, the `sampev` table) installed.
    pub fn new() -> mlua::Result<Self> {
        let lua = Lua::new();
        install_print(&lua)?;
        install_text_codec(&lua)?;
        let outgoing_chat: OutgoingChat = Rc::new(RefCell::new(Vec::new()));
        install_send_chat(&lua, outgoing_chat.clone())?;
        crypto::install_crypto(&lua)?;
        lua.globals().set("sampev", lua.create_table()?)?;
        lua.globals().set("__rakHandlers", lua.create_table()?)?;
        install_register_handler(&lua)?;
        require::install_require(&lua)?;
        // `len` is referenced by `addon.lua` (a latent upstream gap); bind it to byte length.
        lua.globals().set(
            "len",
            lua.create_function(|_, s: mlua::String| Ok(s.as_bytes().len()))?,
        )?;
        let outbox: Outbox = Rc::new(RefCell::new(std::collections::VecDeque::new()));
        Ok(Self {
            lua,
            outgoing_chat,
            outbox: RefCell::new(outbox),
        })
    }

    /// Run an RPC/packet through its `registerHandler` chokepoint: each handler registered under
    /// `chokepoint` is called with `(id, bitStream)` over a copy of `payload`. A handler returning
    /// `false` consumes the packet; any handler that writes to the bitStream rewrites it; otherwise
    /// it passes. This is the RakSAMP-native path the embedded `samp.events` builds on.
    pub fn dispatch_chokepoint(&self, chokepoint: &str, id: u8, payload: &[u8]) -> Verdict {
        let handlers = self.chokepoint_handlers(chokepoint);
        if handlers.is_empty() {
            return Verdict::Pass;
        }
        let stream = bitstream::BitStreamUserData::new(
            samp_proto::RwBitStream::from_bytes(payload.to_vec()),
            self.outbox.borrow().clone(),
        );
        let probe = stream.clone();
        let userdata = match self.lua.create_userdata(stream) {
            Ok(userdata) => userdata,
            Err(error) => {
                tracing::warn!(chokepoint, %error, "failed to build bitStream for handler");
                return Verdict::Pass;
            }
        };
        for handler in handlers {
            match handler.call::<Value>((id, &userdata)) {
                Ok(Value::Boolean(false)) => return Verdict::Drop,
                Ok(_) => {}
                Err(error) => tracing::warn!(chokepoint, %error, "registerHandler error"),
            }
        }
        if probe.is_dirty() {
            Verdict::Rewrite(probe.bytes())
        } else {
            Verdict::Pass
        }
    }

    /// Fire the `registerHandler('onUpdate')` handlers (the task scheduler tick that drives
    /// `newTask`/`wait`). Called from the driver's update timer.
    pub fn dispatch_update(&self) {
        for handler in self.chokepoint_handlers("onUpdate") {
            if let Err(error) = handler.call::<()>(()) {
                tracing::warn!(%error, "onUpdate handler error");
            }
        }
    }

    /// Collect the Lua functions registered under a chokepoint name (in registration order).
    fn chokepoint_handlers(&self, chokepoint: &str) -> Vec<Function> {
        let Ok(table) = self.lua.globals().get::<Table>("__rakHandlers") else {
            return Vec::new();
        };
        match table.get::<Option<Table>>(chokepoint) {
            Ok(Some(list)) => list
                .sequence_values::<Function>()
                .filter_map(Result::ok)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Load and run a chunk, registering whatever globals/callbacks it defines. `name` is used in
    /// Lua error messages.
    pub fn load_script(&self, source: &str, name: &str) -> mlua::Result<()> {
        self.lua.load(source).set_name(name).exec()
    }

    /// Install `sampSendPacket(bytes)` / `sampSendRpc(id, bytes)` bound to `outbox`, letting scripts
    /// queue raw sends the driver will flush. Use the registry's [`samp_proto::Outbox`].
    /// Install the bot getters/setters (`getBot*`/`setBot*`, `getServerAddress`, `updateSync`,
    /// `reconnect`) bound to the shared bot state the driver mirrors.
    pub fn install_bindings(&self, state: samp_client::SharedLocalPlayer) -> mlua::Result<()> {
        bindings::install_bindings(&self.lua, state)
    }

    pub fn install_sender(&self, outbox: Outbox) -> mlua::Result<()> {
        *self.outbox.borrow_mut() = outbox.clone();
        bitstream::install_bitstream(&self.lua, outbox.clone())?;
        let packets = outbox.clone();
        let send_packet = self.lua.create_function(move |_, data: mlua::String| {
            packets.borrow_mut().push_back(OutboundMsg::Packet {
                data: data.as_bytes().to_vec(),
                reliability: samp_proto::RELIABILITY_RELIABLE_ORDERED,
                channel: 0,
            });
            Ok(())
        })?;
        self.lua.globals().set("sampSendPacket", send_packet)?;

        let rpcs = outbox.clone();
        let send_rpc = self
            .lua
            .create_function(move |_, (id, data): (u8, mlua::String)| {
                rpcs.borrow_mut().push_back(OutboundMsg::Rpc {
                    id,
                    payload: data.as_bytes().to_vec(),
                });
                Ok(())
            })?;
        self.lua.globals().set("sampSendRpc", send_rpc)?;

        // `sampSendDialogResponse(id, button, listItem, input)` → RPC_DialogResponse (62), the Lua
        // path for answering login/registration dialogs (replacing the Rust auto-login).
        let dialogs = outbox;
        let send_dialog = self.lua.create_function(
            move |_, (id, button, list_item, input): (u16, u8, u16, mlua::String)| {
                let input = samp_proto::encode_cp1251(&input.to_string_lossy());
                let payload = samp_proto::DialogResponse {
                    dialog_id: id,
                    button,
                    list_item,
                    input: &input,
                }
                .encode();
                dialogs
                    .borrow_mut()
                    .push_back(OutboundMsg::Rpc { id: 62, payload });
                Ok(())
            },
        )?;
        self.lua
            .globals()
            .set("sampSendDialogResponse", send_dialog)
    }

    /// Set a global the script can read (e.g. the configured nick, resolution). Used to hand scripts
    /// the connection context they need (an Arizona script builds its game-path from the nick).
    pub fn set_global(&self, name: &str, value: impl IntoLua) -> mlua::Result<()> {
        self.lua.globals().set(name, value)
    }

    /// Fire a `sampev` callback with fixed args, ignoring its return — for lifecycle events
    /// (`onConnect`, `onInitGame`) the driver dispatches outside the RPC stream.
    pub fn fire(&self, event: &str, args: impl IntoLuaMulti) {
        self.call_event(event, args);
    }

    /// Take and clear the chat lines scripts have queued since the last drain.
    pub fn drain_outgoing_chat(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.outgoing_chat.borrow_mut())
    }

    /// Verdict a player chat broadcast to `sampev.onChatMessage(playerId, text)`. (Chat is decoded
    /// by the driver into a client event rather than reaching the raw-RPC seam.)
    pub fn on_chat(&self, player_id: u16, text: &str) {
        self.call_event("onChatMessage", (player_id, text));
    }

    /// Verdict a server/system message to `sampev.onServerMessage(color, text)`.
    pub fn on_server_message(&self, color: u32, text: &str) {
        self.call_event("onServerMessage", (color, text));
    }

    /// Evaluate an expression and return it as `T` — for tests and host introspection.
    pub fn eval<T: mlua::FromLua>(&self, source: &str) -> mlua::Result<T> {
        self.lua.load(source).eval()
    }

    /// Call a `sampev` handler with fixed args, ignoring its return (used for events the driver
    /// pre-decodes). A missing handler is a no-op; a script error is logged.
    fn call_event(&self, event: &str, args: impl IntoLuaMulti) {
        if let Some(handler) = self.event_handler(event) {
            if let Err(error) = handler.call::<()>(args) {
                tracing::warn!(event, %error, "lua event handler error");
            }
        }
    }

    /// Look up a callable `sampev[event]`.
    fn event_handler(&self, event: &str) -> Option<Function> {
        let sampev: Table = self.lua.globals().get("sampev").ok()?;
        sampev.get::<Option<Function>>(event).ok().flatten()
    }
}

/// `print(...)` → stdout, space-joined, coercing each argument the way Lua's `tostring` would.
fn install_print(lua: &Lua) -> mlua::Result<()> {
    let print = lua.create_function(|lua, args: Variadic<Value>| {
        let parts: Vec<String> = args
            .iter()
            .map(|value| match lua.coerce_string(value.clone()) {
                Ok(Some(s)) => s.to_string_lossy(),
                _ => format!("{value:?}"),
            })
            .collect();
        println!("{}", parts.join("\t"));
        Ok(())
    })?;
    lua.globals().set("print", print)
}

/// Install the cp1251↔UTF-8 helpers (`cp1251ToUtf8`, `utf8ToCp1251`). The high-level `samp.events`
/// string fields and chat/dialog senders use these so scripts always see and send UTF-8; they are
/// also exposed for scripts doing raw bitstream text by hand.
fn install_text_codec(lua: &Lua) -> mlua::Result<()> {
    let to_utf8 = lua.create_function(|lua, bytes: mlua::String| {
        lua.create_string(samp_proto::decode_cp1251(&bytes.as_bytes()))
    })?;
    lua.globals().set("cp1251ToUtf8", to_utf8)?;
    let to_cp1251 = lua.create_function(|lua, text: mlua::String| {
        lua.create_string(samp_proto::encode_cp1251(&text.to_string_lossy()))
    })?;
    lua.globals().set("utf8ToCp1251", to_cp1251)
}

/// `registerHandler(name, fn)` — append `fn` to the chokepoint named `name` (e.g. `"onSendRPC"`).
/// Handlers are stored in the `__rakHandlers` table and run by [`ScriptEngine::dispatch_chokepoint`].
fn install_register_handler(lua: &Lua) -> mlua::Result<()> {
    let register = lua.create_function(|lua, (name, func): (String, Function)| {
        let handlers: Table = lua.globals().get("__rakHandlers")?;
        let list = match handlers.get::<Option<Table>>(name.as_str())? {
            Some(list) => list,
            None => {
                let list = lua.create_table()?;
                handlers.set(name.clone(), list.clone())?;
                list
            }
        };
        list.push(func)?;
        Ok(())
    })?;
    lua.globals().set("registerHandler", register)
}

/// `sampSendChat(text)` — queue a chat line for the host to send (raw bytes, server encoding).
fn install_send_chat(lua: &Lua, outgoing: OutgoingChat) -> mlua::Result<()> {
    let send_chat = lua.create_function(move |_, text: mlua::String| {
        // Scripts speak UTF-8; the wire is cp1251.
        outgoing
            .borrow_mut()
            .push(samp_proto::encode_cp1251(&text.to_string_lossy()));
        Ok(())
    })?;
    lua.globals().set("sampSendChat", send_chat)
}

#[cfg(test)]
mod tests;
