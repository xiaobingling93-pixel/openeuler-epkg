pub mod lrpm;
pub mod lposix;

use std::sync::{LazyLock, Mutex};
use mlua::{Lua, Result as LuaResult};

/// Global cached Lua state with extensions pre-registered.
/// This is lazily initialized on first use and reused across all scriptlet executions.
/// The state is protected by a Mutex to ensure thread safety.
static LUA_STATE_CACHE: LazyLock<Mutex<Lua>> = LazyLock::new(|| {
    let lua = Lua::new();
    // Load standard Lua libraries that RPM Lua scripts expect (all except debug for safety)
    lua.load_std_libs(mlua::StdLib::ALL ^ mlua::StdLib::DEBUG).expect("Failed to load standard libraries");
    // Register extensions once during initialization (lrpm.* and lposix.* namespaces)
    lrpm::register_rpm_extensions(&lua).expect("Failed to register RPM extensions during initialization");
    lposix::register_posix_extensions(&lua).expect("Failed to register POSIX extensions during initialization");
    Mutex::new(lua)
});

/// Get the global cached Lua state with extensions already registered.
/// This is more efficient than creating a new state for each scriptlet execution.
/// The state is thread-safe and can be reused across multiple scriptlet executions.
/// Note: Each scriptlet execution should set up its own `arg` table before executing.
pub fn get_cached_lua_state() -> std::sync::MutexGuard<'static, Lua> {
    LUA_STATE_CACHE.lock().unwrap()
}

/// Setup arg table for scriptlet arguments
/// RPM Lua scriptlets use 1-indexed arguments (Lua convention)
/// arg[1] = scriptlet name (usually empty or scriptlet type)
/// arg[2] = number of installed instances (for install/upgrade/remove)
/// arg[3] = additional arguments (for triggers, etc.)
pub fn setup_arg_table(lua: &Lua, args: &[String]) -> LuaResult<()> {
    let arg_table = lua.create_table()?;

    // Lua uses 1-based indexing
    for (i, arg) in args.iter().enumerate() {
        arg_table.set(i + 1, arg.as_str())?;
    }

    // Set as global 'arg' table
    lua.globals().set("arg", arg_table)?;

    Ok(())
}
