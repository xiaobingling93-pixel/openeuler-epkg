use std::collections::HashMap;
use crate::PackageFormat;

/// Parse provides string based on format and extract names with optional versions
/// Returns a HashMap mapping provide names to optional version strings
///
/// IMPORTANT: Provides are in the form cap_with_arch=version (e.g., "libfoo(x86-64)=2.0").
/// cap_with_arch is an atomic tag that should NEVER be split. This function preserves
/// cap_with_arch when stripping versions, so the returned names are always cap_with_arch
/// (e.g., "libfoo(x86-64)"), not just cap alone. The provide2pkgnames index is keyed
/// by these cap_with_arch values.
///
/// Format-specific examples:
/// - RPM: Items separated by commas. "filesystem = 3.16-6.oe2403sp1, filesystem(x86-64) = 3.16-6.oe2403sp1"
///   -> {"filesystem": "3.16-6.oe2403sp1", "filesystem(x86-64": "3.16-6.oe2403sp1")}
/// - Arch: Items separated by spaces. "libutil-linux libblkid.so=1-64 libfdisk.so=1-64"
///   -> {"libutil-linux": "", "libblkid.so": "1-64", "libfdisk.so": "1-64"}
/// - APK: Items separated by spaces. "pc:gio-2.0=2.84.4 pc:gio-unix-2.0=2.84.4"
///   -> {"pc:gio-2.0": "2.84.4", "pc:gio-unix-2.0": "2.84.4"}
/// - Debian: Items separated by commas with spaces. "node-acorn-bigint (= 1.0.0), node-acorn-class-fields (= 1.0.0)"
///   -> {"node-acorn-bigint": "1.0.0", "node-acorn-class-fields": "1.0.0"}
/// - File paths: "/etc/xdg/autostart" -> {"/etc/xdg/autostart": ""}
fn parse_provides_apk_pacman(provides_str: &str) -> HashMap<String, String> {
    // APK/Pacman: Items separated by whitespace, versions use = directly
    // Example: "pc:gio-2.0=2.84.4 pc:gio-unix-2.0=2.84.4"
    // Also handle library aliases like "libstk-5.0.0.so=libstk-5.0.0.so-64"
    let mut result = HashMap::new();
    for part in provides_str.split_whitespace() {
        if part.is_empty() {
            continue;
        }
        if let Some(equals_pos) = part.find('=') {
            let name = part[..equals_pos].to_string();
            let version = part[equals_pos + 1..].to_string();
            if version.contains(".so") && !version.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                result.insert(name, String::new());
                result.insert(version, String::new());
            } else {
                result.insert(name, version);
            }
        } else {
            result.insert(part.to_string(), String::new());
        }
    }
    result
}

fn parse_provides_deb(provides_str: &str) -> HashMap<String, String> {
    // Debian: Items separated by commas (often ", " but may include newlines/indentation)
    // Example: "node-acorn-bigint (= 1.0.0), node-acorn-class-fields (= 1.0.0)"
    let mut result = HashMap::new();
    for item in provides_str.split(',') {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut provide_name = trimmed.to_string();
        let mut version = String::new();

        if let Some(paren_start) = trimmed.find('(') {
            if trimmed[paren_start..].starts_with("(= ")
                || trimmed[paren_start..].starts_with("(>= ")
                || trimmed[paren_start..].starts_with("(<= ")
                || trimmed[paren_start..].starts_with("(> ")
                || trimmed[paren_start..].starts_with("(< ")
            {
                provide_name = trimmed[..paren_start].trim_end().to_string();
                if let Some(paren_end) = trimmed[paren_start..].find(')') {
                    let version_str = &trimmed[paren_start + 3..paren_start + paren_end].trim();
                    if !version_str.is_empty() {
                        version = version_str.to_string();
                    }
                }
            } else {
                if let Some(equals_pos) = trimmed.find(" = ") {
                    provide_name = trimmed[..equals_pos].trim_end().to_string();
                    version = trimmed[equals_pos + 3..].trim().to_string();
                }
            }
        } else if let Some(equals_pos) = trimmed.find(" = ") {
            provide_name = trimmed[..equals_pos].trim_end().to_string();
            version = trimmed[equals_pos + 3..].trim().to_string();
        }

        if !provide_name.is_empty() {
            result.insert(provide_name, version);
        }
    }
    result
}

fn rpm_token_equals_inside_parens(pos: usize, text: &str) -> bool {
    let before = &text[..pos];
    let mut depth = 0;
    for ch in before.chars() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
        }
    }
    depth > 0
}

fn parse_rpm_provide_item(item: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = item.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let mut provide_name: Option<String> = None;
    let mut version = String::new();

    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if part == &">=" || part == &"<=" || part == &">" || part == &"<" || part == &"=" {
            if idx + 1 < parts.len() {
                version = parts[idx + 1].to_string();
            }
            break;
        } else if let Some(i) = part.find(">=") {
            if !rpm_token_equals_inside_parens(i, part) {
                provide_name = Some(part[..i].to_string());
                version = part[i + 2..].trim().to_string();
                break;
            } else {
                provide_name = Some(part.to_string());
            }
        } else if let Some(i) = part.find("<=") {
            if !rpm_token_equals_inside_parens(i, part) {
                provide_name = Some(part[..i].to_string());
                version = part[i + 2..].trim().to_string();
                break;
            } else {
                provide_name = Some(part.to_string());
            }
        } else if let Some(i) = part.find('>') {
            if !rpm_token_equals_inside_parens(i, part) {
                provide_name = Some(part[..i].to_string());
                version = part[i + 1..].trim().to_string();
                break;
            } else {
                provide_name = Some(part.to_string());
            }
        } else if let Some(i) = part.find('<') {
            if !rpm_token_equals_inside_parens(i, part) {
                provide_name = Some(part[..i].to_string());
                version = part[i + 1..].trim().to_string();
                break;
            } else {
                provide_name = Some(part.to_string());
            }
        } else if let Some(i) = part.find('=') {
            if rpm_token_equals_inside_parens(i, part) {
                provide_name = Some(part.to_string());
            } else {
                provide_name = Some(part[..i].to_string());
                version = part[i + 1..].trim().to_string();
                break;
            }
        } else {
            if provide_name.is_none() {
                provide_name = Some(part.to_string());
            }
        }
    }

    provide_name.map(|name| (name, version))
}

fn parse_provides_rpm(provides_str: &str) -> HashMap<String, String> {
    // RPM: comma-separated list; each item parsed for version operators (whitespace tokens).
    let mut result = HashMap::new();
    for item in provides_str.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let Some((name, version)) = parse_rpm_provide_item(item) {
            result.insert(name, version);
        }
    }
    result
}

/// Epkg, Conda, Python, etc.: whitespace-separated tokens, `name=version` when present.
fn parse_provides_whitespace_equals(provides_str: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for part in provides_str.split_whitespace() {
        if part.is_empty() {
            continue;
        }
        if let Some(i) = part.find('=') {
            let name = part[..i].to_string();
            let version = part[i + 1..].trim().to_string();
            result.insert(name, version);
        } else {
            result.insert(part.to_string(), String::new());
        }
    }
    result
}

pub fn parse_provides(provides_str: &str, format: PackageFormat) -> HashMap<String, String> {
    match format {
        PackageFormat::Apk | PackageFormat::Pacman => parse_provides_apk_pacman(provides_str),
        PackageFormat::Deb => parse_provides_deb(provides_str),
        PackageFormat::Rpm => parse_provides_rpm(provides_str),
        _ => parse_provides_whitespace_equals(provides_str),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_provides_rpm() {
        // RPM: Items separated by commas
        let provides = "filesystem = 3.16-6.oe2403sp1, filesystem(x86-64) = 3.16-6.oe2403sp1, filesystem-afs = 3.16-6.oe2403sp1";
        let result = parse_provides(provides, PackageFormat::Rpm);

        assert_eq!(result.get("filesystem"), Some(&"3.16-6.oe2403sp1".to_string()));
        assert_eq!(result.get("filesystem(x86-64)"), Some(&"3.16-6.oe2403sp1".to_string()));
        assert_eq!(result.get("filesystem-afs"), Some(&"3.16-6.oe2403sp1".to_string()));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_parse_provides_rpm_with_libtcl() {
        // RPM: Complex example with mixed provides
        let provides = "libtcl8.6.so()(64bit), tcl = 1:8.6.14-1.oe2403sp1, tcl(abi) = 8.6, tcl(x86-64) = 1:8.6.14-1.oe2403sp1, tcl-tcldict = 8.6.14";
        let result = parse_provides(provides, PackageFormat::Rpm);

        assert_eq!(result.get("libtcl8.6.so()(64bit)"), Some(&"".to_string()));
        assert_eq!(result.get("tcl"), Some(&"1:8.6.14-1.oe2403sp1".to_string()));
        assert_eq!(result.get("tcl(abi)"), Some(&"8.6".to_string()));
        assert_eq!(result.get("tcl(x86-64)"), Some(&"1:8.6.14-1.oe2403sp1".to_string()));
        assert_eq!(result.get("tcl-tcldict"), Some(&"8.6.14".to_string()));
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_parse_provides_rpm_file_paths() {
        // RPM: File paths (no versions)
        let provides = "/etc/xdg/autostart, /etc/xdg/autostart/polkit-ukui-authentication-agent-1.desktop";
        let result = parse_provides(provides, PackageFormat::Rpm);

        assert_eq!(result.get("/etc/xdg/autostart"), Some(&"".to_string()));
        assert_eq!(result.get("/etc/xdg/autostart/polkit-ukui-authentication-agent-1.desktop"), Some(&"".to_string()));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_provides_pacman() {
        // Arch/Pacman: Items separated by whitespace
        let provides = "libutil-linux libblkid.so=1-64 libfdisk.so=1-64 libmount.so=1-64 libsmartcols.so=1-64 libuuid.so=1-64";
        let result = parse_provides(provides, PackageFormat::Pacman);

        assert_eq!(result.get("libutil-linux"), Some(&"".to_string()));
        assert_eq!(result.get("libblkid.so"), Some(&"1-64".to_string()));
        assert_eq!(result.get("libfdisk.so"), Some(&"1-64".to_string()));
        assert_eq!(result.get("libmount.so"), Some(&"1-64".to_string()));
        assert_eq!(result.get("libsmartcols.so"), Some(&"1-64".to_string()));
        assert_eq!(result.get("libuuid.so"), Some(&"1-64".to_string()));
        assert_eq!(result.len(), 6);
    }

    #[test]
    fn test_parse_provides_apk() {
        // APK: Items separated by whitespace, versions use = directly
        let provides = "pc:gio-2.0=2.84.4 pc:gio-unix-2.0=2.84.4 pc:girepository-2.0=2.84.4 pc:glib-2.0=2.84.4 pc:gmodule-2.0=2.84.4 pc:gmodule-export-2.0=2.84.4 pc:gmodule-no-export-2.0=2.84.4 pc:gobject-2.0=2.84.4 pc:gthread-2.0=2.84.4 cmd:gdbus-codegen=2.84.4-r0 cmd:glib-compile-resources=2.84.4-r0 cmd:glib-genmarshal=2.84.4-r0 cmd:glib-gettextize=2.84.4-r0 cmd:glib-mkenums=2.84.4-r0 cmd:gobject-query=2.84.4-r0 cmd:gresource=2.84.4-r0 cmd:gtester-report=2.84.4-r0 cmd:gtester=2.84.4-r0";
        let result = parse_provides(provides, PackageFormat::Apk);

        assert_eq!(result.get("pc:gio-2.0"), Some(&"2.84.4".to_string()));
        assert_eq!(result.get("pc:gio-unix-2.0"), Some(&"2.84.4".to_string()));
        assert_eq!(result.get("cmd:gdbus-codegen"), Some(&"2.84.4-r0".to_string()));
        assert_eq!(result.get("cmd:gtester"), Some(&"2.84.4-r0".to_string()));
        assert_eq!(result.len(), 18);
    }

    #[test]
    fn test_parse_provides_apk_library_alias() {
        // APK: Library alias handling
        let provides = "libstk-5.0.0.so=libstk-5.0.0.so-64";
        let result = parse_provides(provides, PackageFormat::Apk);

        assert_eq!(result.get("libstk-5.0.0.so"), Some(&"".to_string()));
        assert_eq!(result.get("libstk-5.0.0.so-64"), Some(&"".to_string()));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_provides_deb() {
        // Debian: Items separated by ", " (comma with space), versions use "(= version)" format
        let provides = "node-acorn-bigint (= 1.0.0), node-acorn-class-fields (= 1.0.0), node-acorn-dynamic-import (= 4.0.0), node-acorn-export-ns-from (= 0.2.0), node-acorn-globals (= 6.0.0), node-acorn-import-assertions (= 1.8.0), node-acorn-import-meta (= 1.1.0), node-acorn-jsx (= 5.3.1), node-acorn-loose (= 8.3.0), node-acorn-node (= 2.0.1), node-acorn-numeric-separator (= 0.3.4), node-acorn-private-class-elements (= 1.0.0), node-acorn-private-methods (= 1.0.0), node-acorn-static-class-features (= 1.0.0), node-acorn-walk (= 8.2.0), node-debbundle-acorn (= 8.8.1+ds+~cs25.17.7-2)";
        let result = parse_provides(provides, PackageFormat::Deb);

        assert_eq!(result.get("node-acorn-bigint"), Some(&"1.0.0".to_string()));
        assert_eq!(result.get("node-acorn-class-fields"), Some(&"1.0.0".to_string()));
        assert_eq!(result.get("node-acorn-dynamic-import"), Some(&"4.0.0".to_string()));
        assert_eq!(result.get("node-debbundle-acorn"), Some(&"8.8.1+ds+~cs25.17.7-2".to_string()));
        assert_eq!(result.len(), 16);
    }

    #[test]
    fn test_parse_provides_deb_multiline() {
        // Debian: Items may span multiple lines with indentation
        let provides = "tex4ht (= 2024.20250309-2),\n tex4ht-common (= 2024.20250309-2)";
        let result = parse_provides(provides, PackageFormat::Deb);

        assert_eq!(result.get("tex4ht"), Some(&"2024.20250309-2".to_string()));
        assert_eq!(result.get("tex4ht-common"), Some(&"2024.20250309-2".to_string()));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_parse_provides_deb_with_arch() {
        // Debian: With architecture specification
        let provides = "libfoo (x86_64) = 2.0";
        let result = parse_provides(provides, PackageFormat::Deb);

        assert_eq!(result.get("libfoo (x86_64)"), Some(&"2.0".to_string()));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_parse_provides_empty() {
        // Empty provides string
        let result_rpm = parse_provides("", PackageFormat::Rpm);
        let result_apk = parse_provides("", PackageFormat::Apk);
        let result_deb = parse_provides("", PackageFormat::Deb);
        let result_pacman = parse_provides("", PackageFormat::Pacman);

        assert!(result_rpm.is_empty());
        assert!(result_apk.is_empty());
        assert!(result_deb.is_empty());
        assert!(result_pacman.is_empty());
    }

    #[test]
    fn test_parse_provides_rpm_preserves_arch() {
        // RPM: Ensure cap_with_arch is preserved
        let provides = "libfoo(x86-64) = 2.0, libbar(any) = 1.0";
        let result = parse_provides(provides, PackageFormat::Rpm);

        assert_eq!(result.get("libfoo(x86-64)"), Some(&"2.0".to_string()));
        assert_eq!(result.get("libbar(any)"), Some(&"1.0".to_string()));
        // Should NOT have "libfoo" without arch
        assert!(!result.contains_key("libfoo"));
    }

    #[test]
    fn test_parse_provides_rpm_complex_operators() {
        // RPM: Test various version operators
        let provides = "pkg1 >= 1.0, pkg2 <= 2.0, pkg3 > 0.5, pkg4 < 3.0, pkg5 = 1.2.3";
        let result = parse_provides(provides, PackageFormat::Rpm);

        assert_eq!(result.get("pkg1"), Some(&"1.0".to_string()));
        assert_eq!(result.get("pkg2"), Some(&"2.0".to_string()));
        assert_eq!(result.get("pkg3"), Some(&"0.5".to_string()));
        assert_eq!(result.get("pkg4"), Some(&"3.0".to_string()));
        assert_eq!(result.get("pkg5"), Some(&"1.2.3".to_string()));
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_parse_provides_rpm_font_lang_capability() {
        let provides = "font(:lang=he)";
        let result = parse_provides(provides, PackageFormat::Rpm);

        assert_eq!(result.get("font(:lang=he)"), Some(&"".to_string()));
        assert_eq!(result.len(), 1);
    }

}
