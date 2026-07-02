//! Host crypto primitives and the Arizona `core.asi` attestation resources, exposed to Luau.
//!
//! Luau has no crypto, so the generic `sha256`/`hmacSha256` hashes and the three static
//! `RT_RCDATA` blobs the type-3 anti-cheat challenge samples are provided here; the Arizona
//! attestation *protocol* (parse RPC 186 → reply RPC 187) lives in `luau/arizona/type3.luau`.
//! All values are raw byte strings: hashes are 32 bytes, the resources are opaque blobs.

use hmac::KeyInit;
use hmac::{Hmac, Mac};
use mlua::Lua;
use sha2::{Digest, Sha256};

/// The three `RT_RCDATA` blobs the server samples, extracted from `core.asi` (flag → resource):
/// `0x64` → #100 (32 B), `0x65` → #101 (32 KB), `0xC0` → #57024 (64 B).
const RES_100: &[u8] = include_bytes!("type3_res/res_100.bin");
const RES_101: &[u8] = include_bytes!("type3_res/res_101.bin");
const RES_57024: &[u8] = include_bytes!("type3_res/res_57024.bin");

fn core_resource(flag: u8) -> Option<&'static [u8]> {
    match flag {
        0x64 => Some(RES_100),
        0x65 => Some(RES_101),
        0xC0 => Some(RES_57024),
        _ => None,
    }
}

/// Install `sha256(bytes)`, `hmacSha256(key, msg)`, and `arizonaCoreResource(flag)`.
pub fn install_crypto(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();

    let sha256 = lua.create_function(|lua, data: mlua::String| {
        lua.create_string(Sha256::digest(data.as_bytes()))
    })?;
    globals.set("sha256", sha256)?;

    let hmac_sha256 = lua.create_function(|lua, (key, msg): (mlua::String, mlua::String)| {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&key.as_bytes()).expect("HMAC accepts any key length");
        mac.update(&msg.as_bytes());
        lua.create_string(mac.finalize().into_bytes())
    })?;
    globals.set("hmacSha256", hmac_sha256)?;

    let core_res = lua.create_function(|lua, flag: u8| match core_resource(flag) {
        Some(bytes) => Ok(Some(lua.create_string(bytes)?)),
        None => Ok(None),
    })?;
    globals.set("arizonaCoreResource", core_res)
}
