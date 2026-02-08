//! mtree format parser and generator
//!
//! This module handles BSD mtree format as specified in mtree(5).
//! Supports parsing of mtree files with /set commands, relative paths,
//! and attribute inheritance.

use std::collections::HashMap;
use std::path::PathBuf;
use color_eyre::Result;
use color_eyre::eyre::eyre;

/// File type in mtree format
#[derive(Debug, Clone, PartialEq)]
pub enum MtreeFileType {
    File,
    Dir,
    Link,
    Device,
    Char,
    Block,
    Fifo,
    Socket,
}

/// File information parsed from mtree
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MtreeFileInfo {
    /// Path of the file (unescaped)
    pub path: String,
    /// File type
    pub file_type: MtreeFileType,
    /// Octal mode
    pub mode: Option<u32>,
    /// Size in bytes
    pub size: Option<u64>,
    /// Modification time as seconds.nanoseconds
    pub time: Option<f64>,
    /// SHA‑256 digest (primary)
    pub sha256digest: Option<String>,
    /// Alias for sha256digest (same value)
    pub sha256: Option<String>,
    /// Target of symbolic link (if type=link)
    pub link_target: Option<String>,
    /// Owner name
    pub uname: Option<String>,
    /// Group name
    pub gname: Option<String>,
    /// Owner ID
    pub uid: Option<u32>,
    /// Group ID
    pub gid: Option<u32>,
    /// All keywords as raw strings, including those also parsed into explicit fields (mode, size, time, etc.)
    pub attrs: HashMap<String, String>,
}

impl MtreeFileInfo {
    pub fn is_dir(&self) -> bool {
        self.file_type == MtreeFileType::Dir
    }

    #[allow(dead_code)]
    pub fn is_file(&self) -> bool {
        self.file_type == MtreeFileType::File
    }

    pub fn is_link(&self) -> bool {
        self.file_type == MtreeFileType::Link
    }
}

impl MtreeFileType {
    #[allow(dead_code)]
    fn as_str(&self) -> &'static str {
        match self {
            MtreeFileType::File     => "file",
            MtreeFileType::Dir      => "dir",
            MtreeFileType::Link     => "link",
            MtreeFileType::Char     => "char",
            MtreeFileType::Block    => "block",
            MtreeFileType::Fifo     => "fifo",
            MtreeFileType::Socket   => "socket",
            MtreeFileType::Device   => "device",
        }
    }
}

/// Internal parsing state
#[allow(dead_code)]
struct ParseState {
    /// Current defaults from `/set`
    defaults: HashMap<String, String>,
    /// Current directory for relative entries
    current_dir: PathBuf,
}

impl ParseState {
    fn new() -> Self {
        Self {
            defaults: HashMap::new(),
            current_dir: PathBuf::from("."),
        }
    }

    /// Parse a single line, update state, return entry if any
    fn parse_line(&mut self, line: &str) -> Result<Option<MtreeFileInfo>> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }

        // Split into whitespace‑separated tokens
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let first = tokens.first().ok_or_else(|| eyre!("empty line"))?;

        // Special commands
        if first.starts_with('/') {
            return self.parse_special(line, &tokens);
        }

        // Dotdot entry (changes directory, ignores all keywords)
        if *first == ".." {
            self.current_dir.pop();
            // spec: options on dotdot entries are always ignored
            return Ok(None);
        }

        // Determine if this is a full path (has '/' after first char, not starting with "./")
        let is_full = is_full_path(first);
        // Path token(s) may be multiple words until a key=value appears
        let (path_tokens, kv_start) = split_path_and_keywords(&tokens);
        if path_tokens.is_empty() {
            return Ok(None);
        }

        // Normalize path: strip "./" prefix, join with space, unescape
        let unescaped_path = normalize_path_tokens(&path_tokens);

        // Parse key=value pairs from this line
        let line_attrs = parse_keywords(&tokens[kv_start..])?;

        // Merge defaults with line attributes (line overrides).
        // Note: after merging, there is no distinction between default and line attributes.
        let mut attrs = self.defaults.clone();
        for (k, v) in line_attrs {
            attrs.insert(k, v);
        }
        // Remove empty values (empty string overrides default with unset)
        attrs.retain(|_, v| !v.is_empty());

        // Build entry from merged attributes
        let info = MtreeFileInfo::from_attrs(unescaped_path, attrs)?;

        // Update current directory if this is a relative directory entry
        if !is_full && info.file_type == MtreeFileType::Dir {
            self.current_dir.push(&info.path);
        }

        Ok(Some(info))
    }

    fn parse_special(&mut self, line: &str, tokens: &[&str]) -> Result<Option<MtreeFileInfo>> {
        match tokens[0] {
            "/set" => {
                for part in &tokens[1..] {
                    let (key, value) = part.split_once('=')
                        .ok_or_else(|| eyre!("Invalid key=value pair in /set: {}", part))?;
                    // Empty values are ignored (same as parse_keywords)
                    if !value.is_empty() {
                        self.defaults.insert(key.to_string(), value.to_string());
                    }
                }
                Ok(None)
            }
            "/unset" => {
                for key in &tokens[1..] {
                    self.defaults.remove(*key);
                }
                Ok(None)
            }
            _ => Err(eyre!("Unknown special command: {}", line)),
        }
    }
}

/// Split tokens into path tokens and index where key=value start
///
/// # Example
/// Input: `["doc/Lorem", "ipsum.txt", "type=file", "mode=644"]`
/// Returns: `(["doc/Lorem", "ipsum.txt"], 2)`
/// (path tokens = first 2 elements, key=value start at index 2)
///
/// # Edge cases
/// - No key=value tokens: returns all tokens as path, kv_start = tokens.len()
/// - Spaces in filenames cause additional path tokens (e.g., `"file with spaces.txt"` → `["file", "with", "spaces.txt"]`)
/// - Non‑printable characters are escaped as octal (e.g., `"file\\177"` for DEL character)
fn split_path_and_keywords<'a>(tokens: &'a [&'a str]) -> (Vec<&'a str>, usize) {
    let mut path_tokens = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if token.contains('=') {
            return (path_tokens, i);
        }
        path_tokens.push(*token);
    }
    // No key=value tokens, whole line is path
    (path_tokens, tokens.len())
}

/// Parse key=value tokens into hashmap
/// Note: Values containing spaces must be escaped as \040 (space) or they will be
/// split into separate tokens by split_whitespace(). This is particularly important
/// for link targets containing spaces.
/// Empty values are ignored (not inserted into the map).
fn parse_keywords(tokens: &[&str]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for token in tokens {
        let (key, value) = token.split_once('=')
            .ok_or_else(|| eyre!("Invalid key=value pair: {}", token))?;
        if !value.is_empty() {
            map.insert(key.to_string(), value.to_string());
        }
    }
    Ok(map)
}

/// Strip leading "./" from a token if present
fn strip_dot_slash(token: &str) -> &str {
    if token.starts_with("./") {
        &token[2..]
    } else {
        token
    }
}

/// Normalize path tokens: strip "./" from first token, join with space, unescape
fn normalize_path_tokens(path_tokens: &[&str]) -> String {
    if path_tokens.is_empty() {
        return String::new();
    }
    let mut normalized: Vec<&str> = path_tokens.to_vec();
    // Strip "./" only from the first token
    normalized[0] = strip_dot_slash(normalized[0]);
    let escaped = normalized.join(" ");
    unescape_mtree_path(&escaped)
}

/// Determine if a path token represents a full path (has '/' after first char, not starting with "./")
fn is_full_path(token: &str) -> bool {
    token.contains('/') && !token.starts_with("./")
}

impl MtreeFileInfo {
    /// Construct from path and merged attributes hashmap
    fn from_attrs(path: String, attrs: HashMap<String, String>) -> Result<Self> {
        let file_type = parse_file_type(attrs.get("type").map(|s| s.as_str()).unwrap_or("file"));
        let mode = attrs.get("mode").and_then(|s| u32::from_str_radix(s, 8).ok());
        let size = attrs.get("size").and_then(|s| s.parse().ok());
        let time = attrs.get("time").and_then(|s| s.parse().ok());
        let sha256digest = attrs.get("sha256digest")
            .or_else(|| attrs.get("sha256"))
            .cloned();
        let sha256 = sha256digest.clone();
        // Link targets are unescaped here. Note: link targets containing unescaped spaces
        // cannot be parsed correctly because spaces separate tokens in mtree format.
        // Our escape_mtree_path() doesn't escape spaces (following the specification),
        // so link targets with spaces written by this implementation will be unparseable.
        // Link targets with spaces are effectively not supported unless spaces are
        // escaped as \040 (which violates the specification but works in practice).
        let link_target = attrs.get("link").map(|s| unescape_mtree_path(s));
        let uname = attrs.get("uname").cloned();
        let gname = attrs.get("gname").cloned();
        let uid = attrs.get("uid").and_then(|s| s.parse().ok());
        let gid = attrs.get("gid").and_then(|s| s.parse().ok());

        Ok(MtreeFileInfo {
            path,
            file_type,
            mode,
            size,
            time,
            sha256digest,
            sha256,
            link_target,
            uname,
            gname,
            uid,
            gid,
            attrs,
        })
    }
}

fn parse_file_type(type_str: &str) -> MtreeFileType {
    match type_str {
        "file"      => MtreeFileType::File,
        "dir"       => MtreeFileType::Dir,
        "link"      => MtreeFileType::Link,
        "char"      => MtreeFileType::Char,
        "block"     => MtreeFileType::Block,
        "fifo"      => MtreeFileType::Fifo,
        "socket"    => MtreeFileType::Socket,
        _           => MtreeFileType::Device,
    }
}

/// Parse a complete mtree file with state tracking
#[allow(dead_code)]
pub fn parse_mtree(content: &str) -> Result<Vec<MtreeFileInfo>> {
    let mut state = ParseState::new();
    let mut results = Vec::new();

    for line in content.lines() {
        if let Some(info) = state.parse_line(line)? {
            results.push(info);
        }
    }

    Ok(results)
}

/// Simplified parser for epkg's filelist.txt format (no /set, no directory tracking)
pub fn parse_simplified_mtree(content: &str) -> Result<Vec<MtreeFileInfo>> {
    let mut results = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let tokens: Vec<&str> = line.split_whitespace().collect();
        let (path_tokens, kv_start) = split_path_and_keywords(&tokens);
        if path_tokens.is_empty() {
            continue;
        }

        let unescaped_path = normalize_path_tokens(&path_tokens);
        let line_attrs = parse_keywords(&tokens[kv_start..])?;

        let info = MtreeFileInfo::from_attrs(unescaped_path, line_attrs)?;
        results.push(info);
    }

    Ok(results)
}

/// Escape a path for mtree format according to mtree(5) specification.
/// Encodes backslash and characters outside the 95 printable ASCII range (0x20-0x7E)
/// as backslash followed by three octal digits.
pub fn escape_mtree_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    for ch in path.chars() {
        let byte = ch as u8;
        // Always escape backslash (0x5C = 134 octal)
        if ch == '\\' {
            result.push_str("\\134");
        }
        // Escape characters outside printable ASCII (0x20-0x7E)
        // Space (0x20) is printable and SHOULD NOT be escaped per spec.
        // Note: Some implementations escape spaces (as \040) to avoid delimiter
        // ambiguity in mtree format, but this violates the specification.
        // WARNING: Not escaping spaces means link targets containing spaces will
        // produce unparseable mtree output, as spaces separate tokens in the format.
        // Link targets with spaces are effectively not supported in this implementation.
        // Alternative: escape spaces as \040 to support link targets with spaces,
        // but this violates the mtree specification.
        else if byte < 0x20 || byte > 0x7E {
            result.push_str(&format!("\\{:03o}", byte));
        }
        else {
            result.push(ch);
        }
    }
    result
}

/// Unescape a path from mtree format according to mtree(5) specification.
/// Decodes backslash followed by three octal digits to the corresponding character.
/// Handles mixed escaped/unescaped input for backward compatibility.
pub fn unescape_mtree_path(escaped_path: &str) -> String {
    let bytes = escaped_path.as_bytes();
    let mut result = String::with_capacity(escaped_path.len());
    let mut i = 0;
    let len = bytes.len();

    #[inline]
    fn is_octal(b: u8) -> bool {
        b >= b'0' && b <= b'7'
    }

    while i < len {
        if bytes[i] == b'\\' && i + 3 < len {
            let d0 = bytes[i + 1];
            let d1 = bytes[i + 2];
            let d2 = bytes[i + 3];
            if is_octal(d0) && is_octal(d1) && is_octal(d2) {
                let val = (d0 - b'0') * 64 + (d1 - b'0') * 8 + (d2 - b'0');
                result.push(val as char);
                i += 4;
                continue;
            }
        }
        // fallback: push current byte as char (ASCII guaranteed)
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_path_and_keywords() {
        // Basic case with full path
        let tokens = ["usr/bin/bash", "type=file", "mode=755"];
        let (path_tokens, kv_start) = split_path_and_keywords(&tokens);
        assert_eq!(path_tokens, &["usr/bin/bash"]);
        assert_eq!(kv_start, 1);

        // Realistic case with directory and spaced filename
        let tokens = ["doc/Lorem", "ipsum.txt", "type=file", "mode=644"];
        let (path_tokens, kv_start) = split_path_and_keywords(&tokens);
        assert_eq!(path_tokens, &["doc/Lorem", "ipsum.txt"]);
        assert_eq!(kv_start, 2);

        // Edge case: single path with no attributes
        let tokens = ["var/log/syslog"];
        let (path_tokens, kv_start) = split_path_and_keywords(&tokens);
        assert_eq!(path_tokens, &["var/log/syslog"]);
        assert_eq!(kv_start, 1);

        // Edge case: empty tokens
        let tokens: [&str; 0] = [];
        let (path_tokens, kv_start) = split_path_and_keywords(&tokens);
        assert!(path_tokens.is_empty());
        assert_eq!(kv_start, 0);
    }

    #[test]
    fn test_escape_unescape() {
        let path = "file with spaces.txt";
        let escaped = escape_mtree_path(path);
        // Space should NOT be escaped
        assert_eq!(escaped, "file with spaces.txt");

        // Test backslash
        let path2 = "file\\with\\backslash";
        let escaped2 = escape_mtree_path(path2);
        assert_eq!(escaped2, "file\\134with\\134backslash");

        let unescaped2 = unescape_mtree_path(&escaped2);
        assert_eq!(unescaped2, path2);

        // Test non‑ASCII (byte > 0x7E)
        let path3 = "file\x7f"; // DEL character
        let escaped3 = escape_mtree_path(path3);
        assert_eq!(escaped3, "file\\177");
        assert_eq!(unescape_mtree_path(&escaped3), path3);
    }

    #[test]
    fn test_parse_simplified() {
        let content = "usr/bin/bash type=file mode=755 sha256digest=abc123";
        let results = parse_simplified_mtree(content).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "usr/bin/bash");
        assert!(results[0].is_file());
        assert_eq!(results[0].mode, Some(0o755));
        assert_eq!(results[0].sha256digest, Some("abc123".to_string()));
        assert_eq!(results[0].sha256, Some("abc123".to_string()));
    }

    #[test]
    fn test_parse_full_mtree() {
        let content = r#"#mtree
/set type=file uid=0 gid=0 mode=644
./.BUILDINFO time=1765404175.0 size=5292 sha256digest=abc123
./etc type=dir mode=755
/set mode=755
./usr type=dir
./usr/bin/bash type=file mode=755 sha256digest=def456"#;

        let results = parse_mtree(content).unwrap();
        assert_eq!(results.len(), 4);

        // First entry should have defaults applied
        assert_eq!(results[0].path, ".BUILDINFO");
        assert_eq!(results[0].mode, Some(0o644));
        assert_eq!(results[0].uid, Some(0));
        assert_eq!(results[0].gid, Some(0));

        // Directory entry
        assert_eq!(results[1].path, "etc");
        assert!(results[1].is_dir());
        assert_eq!(results[1].mode, Some(0o755));

        // Directory entry (usr)
        assert_eq!(results[2].path, "usr");
        assert!(results[2].is_dir());
        assert_eq!(results[2].mode, Some(0o755)); // inherits from previous /set mode=755

        // File with new mode default
        assert_eq!(results[3].path, "usr/bin/bash");
        assert_eq!(results[3].mode, Some(0o755));
    }

    #[test]
    fn test_full_vs_relative_paths() {
        let content = r#"
usr/bin/bash type=file
./usr/local/bin/zsh type=file
"#;
        let results = parse_mtree(content).unwrap();
        assert_eq!(results.len(), 2);
        // first is full path (contains '/'), second is relative (starts with ./)
        // both should be parsed correctly
        assert_eq!(results[0].path, "usr/bin/bash");
        assert_eq!(results[1].path, "usr/local/bin/zsh");
    }

    #[test]
    fn test_dotdot() {
        let content = r#"
./dir1 type=dir
./dir1/file1 type=file
..
./dir2 type=dir
"#;
        let results = parse_mtree(content).unwrap();
        // dotdot should not produce an entry
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].path, "dir1");
        assert_eq!(results[1].path, "dir1/file1");
        // after dotdot, current directory is back to ".", so dir2 is relative
        assert_eq!(results[2].path, "dir2");
    }
}
