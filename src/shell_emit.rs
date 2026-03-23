//! Shell-specific emission for `export` / `$env:` lines used by `epkg env` eval.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    PowerShell,
}

/// Select via `EPKG_SHELL`: `powershell` / `pwsh` → PowerShell; anything else → bash-style `export`.
pub fn detect() -> ShellKind {
    match std::env::var("EPKG_SHELL").ok().as_deref() {
        Some("powershell") | Some("pwsh") => ShellKind::PowerShell,
        _ => ShellKind::Bash,
    }
}

/// Host PATH separator (`:` on Unix, `;` on Windows).
pub fn path_sep_os() -> char {
    if cfg!(windows) {
        ';'
    } else {
        ':'
    }
}

pub fn ps_escape_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

pub fn emit_export(key: &str, value: &str, kind: ShellKind) -> String {
    match kind {
        ShellKind::Bash => {
            format!("export {}=\"{}\"", key, value.replace('\\', "\\\\").replace('"', "\\\""))
        }
        ShellKind::PowerShell => {
            format!(
                "$env:{} = '{}'",
                key,
                ps_escape_single_quoted(value)
            )
        }
    }
}

pub fn emit_path(path: &str, kind: ShellKind) -> String {
    match kind {
        ShellKind::Bash => {
            format!("export PATH=\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
        }
        ShellKind::PowerShell => {
            format!(
                "$env:PATH = '{}'",
                ps_escape_single_quoted(path)
            )
        }
    }
}

pub fn deactivate_script_extension(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Bash => "sh",
        ShellKind::PowerShell => "ps1",
    }
}
