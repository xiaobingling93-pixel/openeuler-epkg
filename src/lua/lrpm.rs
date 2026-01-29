use mlua::{Lua, Result as LuaResult, Value, MultiValue};
use crate::version_compare;
use crate::lua::lposix::{pushresult, pusherror_with_code};
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

    // rpm.spawn(args, options) - spawn a process with optional redirections
    // args: table of command arguments (first is the program to execute)
    // options: optional table with redirections (stdin, stdout, stderr)
    // Returns: exit code on success, (nil, error_string, errno) on failure
    rpm_table.set("spawn", lua.create_function(|lua, (args_table, options): (mlua::Table, Option<mlua::Table>)| -> LuaResult<MultiValue> {
        use std::ffi::CString;

        // Get argument count and validate - raise RuntimeError for validation errors (pcall catches these)
        let argc = args_table.len().map_err(|e| e)? as i32;
        if argc == 0 {
            return Err(mlua::Error::RuntimeError("command not supplied".to_string()));
        }

        // Build argv vector - store owned CStrings to keep data alive
        let mut argv_strings: Vec<CString> = Vec::new();
        for i in 1..=argc as i64 {
            let arg = match args_table.get(i)? {
                Value::String(s) => s.to_str()?.to_string(),
                _ => return Err(mlua::Error::RuntimeError("all arguments must be strings".to_string())),
            };
            let arg_cstr = CString::new(arg)
                .map_err(|_| mlua::Error::RuntimeError("argument contains null byte".to_string()))?;
            argv_strings.push(arg_cstr);
        }

        // Build argv pointer array (references must stay valid)
        // posix_spawnp expects *const *mut i8 (array of mutable pointers, const array)
        // The array must be NULL-terminated
        let mut argv_ptrs: Vec<*mut i8> = Vec::with_capacity(argv_strings.len() + 1);
        for s in &argv_strings {
            argv_ptrs.push(s.as_ptr() as *mut i8);
        }
        argv_ptrs.push(std::ptr::null_mut()); // NULL terminator for argv

        // Set up file actions for redirections
        let mut fa = std::mem::MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
        let mut fa_initialized = false;

        // Keep CString for paths alive during the spawn call
        let mut path_cstrings: Vec<CString> = Vec::new();

        if let Some(opts) = options {
            let rc = unsafe { libc::posix_spawn_file_actions_init(fa.as_mut_ptr()) };
            if rc != 0 {
                return pusherror_with_code(lua, None, None);
            }
            fa_initialized = true;

            // Process each redirection option - raise RuntimeError for invalid directives
            let mut pairs = opts.pairs::<mlua::Value, mlua::Value>();
            while let Some(pair) = pairs.next() {
                let (key, val) = pair?;
                let key_str = match key {
                    Value::String(s) => s.to_str()?.to_string(),
                    _ => return Err(mlua::Error::RuntimeError("invalid spawn directive key type".to_string())),
                };
                let val_str = match val {
                    Value::String(s) => s.to_str()?.to_string(),
                    _ => return Err(mlua::Error::RuntimeError("invalid spawn directive value type".to_string())),
                };

                let val_cstr = CString::new(val_str)
                    .map_err(|_| mlua::Error::RuntimeError("path contains null byte".to_string()))?;
                path_cstrings.push(val_cstr.clone());

                let rc = match key_str.as_str() {
                    "stdin" => {
                        unsafe { libc::posix_spawn_file_actions_addopen(fa.as_mut_ptr(), 0, val_cstr.as_ptr(), libc::O_RDONLY, 0o644) }
                    }
                    "stdout" => {
                        unsafe { libc::posix_spawn_file_actions_addopen(fa.as_mut_ptr(), 1, val_cstr.as_ptr(), libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT, 0o644) }
                    }
                    "stderr" => {
                        unsafe { libc::posix_spawn_file_actions_addopen(fa.as_mut_ptr(), 2, val_cstr.as_ptr(), libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT, 0o644) }
                    }
                    _ => return Err(mlua::Error::RuntimeError(format!("invalid spawn directive: {}", key_str))),
                };

                if rc != 0 {
                    unsafe { libc::posix_spawn_file_actions_destroy(fa.as_mut_ptr()) };
                    return pusherror_with_code(lua, None, None);
                }
            }
        }

        // Spawn the process
        let mut pid: libc::pid_t = 0;
        let fap = if fa_initialized {
            fa.as_mut_ptr()
        } else {
            std::ptr::null_mut()
        };
        let rc = unsafe {
            libc::posix_spawnp(
                &mut pid,
                argv_ptrs[0],
                fap,
                std::ptr::null(),
                argv_ptrs.as_ptr(),
                std::ptr::null()
            )
        };

        // Clean up file actions
        if fa_initialized {
            unsafe { libc::posix_spawn_file_actions_destroy(fa.as_mut_ptr()) };
        }

        if rc != 0 {
            return pusherror_with_code(lua, None, None);
        }

        // Wait for the process to complete
        let mut status: i32 = 0;
        let wait_rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if wait_rc == -1 {
            return pusherror_with_code(lua, None, None);
        }

        if status != 0 {
            if libc::WIFSIGNALED(status) {
                let signal = libc::WTERMSIG(status);
                let sig_names = ["HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "BUS", "FPE", "KILL", "USR1", "SEGV", "USR2", "PIPE", "ALRM", "TERM", "STKFLT", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU", "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "IO", "PWR", "SYS"];
                let name = sig_names.get(signal as usize - 1).unwrap_or(&"UNKNOWN");
                return pusherror_with_code(lua, Some(&format!("exit signal {} ({})", signal, name)), Some(signal));
            } else {
                let exit_code = libc::WEXITSTATUS(status);
                return pusherror_with_code(lua, Some(&format!("exit code {}", exit_code)), Some(exit_code));
            }
        }

        // Return exit status (which should be 0 for success)
        pushresult(lua, libc::WEXITSTATUS(status), None)
    })?)?;

    // Register as global 'rpm' table
    lua.globals().set("rpm", rpm_table)?;

    Ok(())
}
