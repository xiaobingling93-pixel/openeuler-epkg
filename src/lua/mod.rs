pub mod lrpm;
pub mod lposix;

use std::sync::{LazyLock, Mutex};
use mlua::{Lua, Table};

/// Global cached Lua state with extensions pre-registered.
/// This is lazily initialized on first use and reused across all scriptlet executions.
/// The state is protected by a Mutex to ensure thread safety.
static LUA_STATE_CACHE: LazyLock<Mutex<Lua>> = LazyLock::new(|| {
    let lua = Lua::new();
    // Load standard Lua libraries that RPM Lua scripts expect (all except debug for safety)
    lua.load_std_libs(mlua::StdLib::ALL ^ mlua::StdLib::DEBUG).expect("Failed to load standard libraries");
    // Register extensions once during initialization (rpm.* and posix.* namespaces)
    lrpm::register_rpm_extensions(&lua).expect("Failed to register RPM extensions during initialization");
    lposix::register_posix_extensions(&lua).expect("Failed to register POSIX extensions during initialization");
    // Also register posix and rpm modules in package.preload so require('posix') and require('rpm') work
    // This matches the C++ RPM implementation behavior where modules are registered via luaL_register.
    // require() expects package.preload[modname] to be a function that returns the module value.
    let globals = lua.globals();
    let package: Table = globals.get("package").unwrap();
    let preload: Table = package.get("preload").unwrap();
    let posix: Table = globals.get("posix").unwrap();
    // Wrap the posix table in a function that returns it (matching luaL_register behavior)
    // The function receives (context, ...) args and returns the module
    let posix_loader = lua.create_function(move |_, ()| Ok(posix.clone())).unwrap();
    preload.set("posix", posix_loader).unwrap();
    // Also register rpm module in package.preload
    let rpm_table: Table = globals.get("rpm").unwrap();
    let rpm_loader = lua.create_function(move |_, ()| Ok(rpm_table.clone())).unwrap();
    preload.set("rpm", rpm_loader).unwrap();
    Mutex::new(lua)
});

/// Get the global cached Lua state with extensions already registered.
/// This is more efficient than creating a new state for each scriptlet execution.
/// The state is thread-safe and can be reused across multiple scriptlet executions.
/// Note: Each scriptlet execution should set up its own `arg` table before executing.
pub fn get_cached_lua_state() -> std::sync::MutexGuard<'static, Lua> {
    LUA_STATE_CACHE.lock().unwrap()
}
