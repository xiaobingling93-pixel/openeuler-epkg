use mlua::{Lua, Result as LuaResult};
use crate::version_compare;

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

    // Register as global 'rpm' table
    lua.globals().set("rpm", rpm_table)?;

    Ok(())
}
