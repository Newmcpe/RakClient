//! RakSAMP native bot getters/setters, backed by the shared [`SharedBotState`] the driver mirrors.
//!
//! State (position/rotation/nick/…) reads and writes go straight to the shared cell; `updateSync`
//! and `reconnect` set flags the driver polls. Money/interior/vehicle/camera are placeholders until
//! the relevant RPCs populate them (Phase 6).

use mlua::Lua;
use samp_client::SharedBotState;
use samp_proto::Vector3;

/// Install `getBot*`/`setBot*`, `getServerAddress`, `updateSync`, `reconnect` bound to `state`.
pub fn install_bindings(lua: &Lua, state: SharedBotState) -> mlua::Result<()> {
    let globals = lua.globals();

    let s = state.clone();
    globals.set(
        "getBotNick",
        lua.create_function(move |_, ()| Ok(s.borrow().nick.clone()))?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotNick",
        lua.create_function(move |_, nick: String| {
            s.borrow_mut().nick = nick;
            Ok(())
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "getServerAddress",
        lua.create_function(move |_, ()| Ok(s.borrow().server_addr.to_string()))?,
    )?;

    let s = state.clone();
    globals.set(
        "getBotPosition",
        lua.create_function(move |_, ()| {
            let p = s.borrow().position;
            Ok((p.x, p.y, p.z))
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotPosition",
        lua.create_function(move |_, (x, y, z): (f32, f32, f32)| {
            s.borrow_mut().position = Vector3 { x, y, z };
            Ok(())
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "getBotRotation",
        lua.create_function(move |_, ()| {
            // Yaw (Z) from the on-foot quaternion, in degrees.
            let q = s.borrow().rotation;
            let yaw = (2.0 * (q.w * q.z + q.x * q.y))
                .atan2(1.0 - 2.0 * (q.y * q.y + q.z * q.z))
                .to_degrees();
            Ok(yaw)
        })?,
    )?;

    let s = state.clone();
    globals.set(
        "getBotVehicle",
        lua.create_function(move |_, ()| Ok(s.borrow().vehicle as i32))?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotVehicle",
        lua.create_function(move |_, (id, _seat): (u16, i32)| {
            // TODO(phase6): actually enter the vehicle (RPC); for now just record the id.
            s.borrow_mut().vehicle = id;
            Ok(())
        })?,
    )?;

    let s = state.clone();
    globals.set(
        "getBotMoney",
        lua.create_function(move |_, ()| Ok(s.borrow().money))?,
    )?;
    let s = state.clone();
    globals.set(
        "getBotInterior",
        lua.create_function(move |_, ()| Ok(s.borrow().interior as i32))?,
    )?;
    let s = state.clone();
    globals.set(
        "getBotCameraPos",
        lua.create_function(move |_, ()| {
            let p = s.borrow().camera_pos;
            Ok((p.x, p.y, p.z))
        })?,
    )?;

    let s = state.clone();
    globals.set(
        "updateSync",
        lua.create_function(move |_, ()| {
            s.borrow_mut().force_sync = true;
            Ok(())
        })?,
    )?;
    let s = state;
    globals.set(
        "reconnect",
        lua.create_function(move |_, ms: u64| {
            s.borrow_mut().reconnect_in_ms = Some(ms);
            Ok(())
        })?,
    )?;

    Ok(())
}
