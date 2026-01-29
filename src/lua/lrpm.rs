use mlua::{Lua, Result as LuaResult};
use crate::version_compare;
use glob::glob;

/// Compatible with /c/rpm-software-management/rpm/rpmio/rpmlua.cc lua funcs in rpmlib[]

/// Register rpm.* namespace functions
pub fn register_rpm_extensions(lua: &Lua) -> LuaResult<()> {
    let rpm_table = lua.create_table()?;

    // rpm.vercmp(v1, v2) - Version comparison
    // Returns: -1 if v1 < v2, 0 if v1 == v2, 1 if v1 > v2
    rpm_table.set("vercmp", lua.create_function(|_lua, (v1, v2): (String, String)| {
        use std::cmp::Ordering;
        use crate::models::PackageFormat;

        // Use epkg's version comparison for RPM format
        match version_compare::compare_versions(&v1, &v2, PackageFormat::Rpm) {
            Some(Ordering::Less) => Ok(-1),
            Some(Ordering::Equal) => Ok(0),
            Some(Ordering::Greater) => Ok(1),
            None => {
                // If parsing fails, fall back to string comparison
                // This matches RPM's behavior for invalid versions
                Ok(v1.cmp(&v2) as i32)
            }
        }
    })?)?;

    // rpm.glob(pattern, flags?) - Glob pattern matching
    // Returns: A Lua table of matching paths (empty table if no matches)
    // flags: Optional string containing 'c' for NOCHECK (return pattern if no match)
    rpm_table.set("glob", lua.create_function(|lua, (pattern, flags): (String, Option<String>)| -> LuaResult<mlua::Value> {
        let no_check = flags.as_ref().map_or(false, |f| f.contains('c'));

        // Perform glob pattern matching
        match glob(&pattern) {
            Ok(entries) => {
                // Create a result table to store matched paths
                let result = lua.create_table()?;
                let mut count = 0;

                for entry in entries.filter_map(|e| e.ok()) {
                    if let Some(path_str) = entry.to_str() {
                        result.set(count + 1, path_str)?;
                        count += 1;
                    }
                }

                // If no matches and NOCHECK is set, return pattern
                if count == 0 && no_check {
                    result.set(1, pattern)?;
                }

                // Always return a table (matches or empty), like rpmGlobPath behavior
                Ok(mlua::Value::Table(result))
            }
            Err(e) => {
                // If glob pattern is invalid, raise Lua error (like luaL_error)
                Err(mlua::Error::RuntimeError(format!("glob {} failed: {}", pattern, e)))
            }
        }
    })?)?;

    // Register as global 'rpm' table
    lua.globals().set("rpm", rpm_table)?;

    Ok(())
}
