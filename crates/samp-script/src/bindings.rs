//! RakSAMP native bot getters/setters, backed by the shared [`SharedLocalPlayer`] the driver mirrors.
//!
//! State (position/rotation/nick/…) reads and writes go straight to the shared cell; `updateSync`
//! and `reconnect` set flags the driver polls. Money/interior/camera are populated from the relevant
//! RPCs by the driver's `track_state`.

use mlua::Lua;
use samp_client::{InVehicleData, SharedLocalPlayer};
use samp_proto::Vector3;

/// Install `getBot*`/`setBot*`, `getServerAddress`, `updateSync`, `reconnect` bound to `state`.
pub fn install_bindings(lua: &Lua, state: SharedLocalPlayer) -> mlua::Result<()> {
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
            let p = s.borrow().on_foot.position;
            Ok((p.x, p.y, p.z))
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotPosition",
        lua.create_function(move |_, (x, y, z): (f32, f32, f32)| {
            s.borrow_mut().on_foot.position = Vector3 { x, y, z };
            Ok(())
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotVelocity",
        lua.create_function(move |_, (x, y, z): (f32, f32, f32)| {
            s.borrow_mut().on_foot.move_speed = Vector3 { x, y, z };
            Ok(())
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotKeys",
        lua.create_function(move |_, keys: u16| {
            // Set the on-foot key bitmask the next sync reports; the server turns it into
            // OnPlayerKeyStateChange. Pulse (mask → updateSync → 0 → updateSync) to emulate a press.
            s.borrow_mut().on_foot.keys = keys;
            Ok(())
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "getBotKeys",
        lua.create_function(move |_, ()| Ok(s.borrow().on_foot.keys))?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotAnimation",
        lua.create_function(move |_, (id, flags): (u16, u16)| {
            let mut bot = s.borrow_mut();
            bot.on_foot.animation_id = id;
            bot.on_foot.animation_flags = flags;
            Ok(())
        })?,
    )?;
    let s = state.clone();
    globals.set(
        "getBotRotation",
        lua.create_function(move |_, ()| {
            // Yaw (Z) from the on-foot quaternion, in degrees.
            let q = s.borrow().on_foot.quaternion;
            let yaw = (2.0 * (q.w * q.z + q.x * q.y))
                .atan2(1.0 - 2.0 * (q.y * q.y + q.z * q.z))
                .to_degrees();
            Ok(yaw)
        })?,
    )?;

    let s = state.clone();
    globals.set(
        "getBotVehicle",
        lua.create_function(move |_, ()| Ok(s.borrow().vehicle_id() as i32))?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotVehicle",
        lua.create_function(move |_, (id, seat): (u16, i32)| {
            // id 0 means "on foot"; otherwise record the vehicle the script put us in.
            let mut bot = s.borrow_mut();
            bot.vehicle = (id != 0).then(|| InVehicleData {
                id,
                seat: seat as u8,
                ..InVehicleData::default()
            });
            Ok(())
        })?,
    )?;

    let s = state.clone();
    globals.set(
        "getBotWeapon",
        lua.create_function(move |_, ()| Ok(s.borrow().weapons.current as i32))?,
    )?;
    let s = state.clone();
    globals.set(
        "setBotWeapon",
        // Arm a weapon so the on-foot sync reports it (e.g. the rented chainsaw, id 9). `give` puts it
        // in its slot AND makes it current; the driver's `adjust_on_foot` reports `weapons.current`.
        // Server-side interactions that require the weapon in hand (the лесопилка chop) then fire.
        lua.create_function(move |_, (id, ammo): (u8, Option<u16>)| {
            s.borrow_mut().weapons.give(id, ammo.unwrap_or(1));
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
    let s = state.clone();
    globals.set(
        "sampSpawnPlayer",
        lua.create_function(move |_, ()| {
            s.borrow_mut().spawn_requested = true;
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
