//! `require(name)` resolver serving the embedded typed-Luau SAMP.Lua port.
//!
//! Stock scripts `require 'samp.events'` etc.; we map module names to `.luau` sources embedded at
//! build time, execute each once, cache the returned value, and hand it back. Modules execute
//! lazily on first `require`, by which point the host bindings (`bitStream`, `registerHandler`, the
//! bot getters, …) are installed.

use std::collections::HashMap;

use mlua::{Lua, Table, Value};

/// `(module name, embedded source)` for the ported SAMP.Lua library.
const MODULES: &[(&str, &str)] = &[
    ("sampfuncs", include_str!("../luau/sampfuncs.luau")),
    ("vector3d", include_str!("../luau/vector3d.luau")),
    ("addon", include_str!("../luau/addon.luau")),
    ("packet", include_str!("../luau/packet.luau")),
    ("fly", include_str!("../luau/fly.luau")),
    ("arizona", include_str!("../luau/arizona.luau")),
    ("arizona.type3", include_str!("../luau/arizona/type3.luau")),
    ("samp.raknet", include_str!("../luau/samp/raknet.luau")),
    ("samp.events", include_str!("../luau/samp/events.luau")),
    (
        "samp.events.core",
        include_str!("../luau/samp/events/core.luau"),
    ),
    (
        "samp.events.bitstream_io",
        include_str!("../luau/samp/events/bitstream_io.luau"),
    ),
    (
        "samp.events.handlers",
        include_str!("../luau/samp/events/handlers.luau"),
    ),
    (
        "samp.events.utils",
        include_str!("../luau/samp/events/utils.luau"),
    ),
    (
        "samp.events.extra_types",
        include_str!("../luau/samp/events/extra_types.luau"),
    ),
];

/// Install the custom `require` global backed by [`MODULES`].
pub fn install_require(lua: &Lua) -> mlua::Result<()> {
    let modules: HashMap<&'static str, &'static str> = MODULES.iter().copied().collect();
    let cache = lua.create_table()?;
    let require = lua.create_function(move |lua, name: String| -> mlua::Result<Value> {
        let cache: Table = lua.named_registry_value("__moduleCache")?;
        if let Some(cached) = cache.get::<Option<Value>>(name.as_str())? {
            if !cached.is_nil() {
                return Ok(cached);
            }
        }
        let Some(source) = modules.get(name.as_str()) else {
            return Err(mlua::Error::runtime(format!("module '{name}' not found")));
        };
        let value: Value = lua.load(*source).set_name(name.as_str()).eval()?;
        cache.set(name.clone(), value.clone())?;
        Ok(value)
    })?;
    lua.set_named_registry_value("__moduleCache", cache)?;
    lua.globals().set("require", require)
}
