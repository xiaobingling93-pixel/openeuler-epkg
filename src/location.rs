use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
use crate::models::dirs;
use color_eyre::eyre::{Context, Result, eyre};
use std::fs;
use ureq;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use std::net::Ipv4Addr;
use std::process;
use std::io::Write;

#[derive(Deserialize)]
struct IpInfo {
    country_code: String,
}

#[derive(Serialize, Deserialize)]
struct CountryCodeCache {
    country_code: String,
    timestamp: u64,
    lan_hash: String,
}

impl CountryCodeCache {
    const CACHE_DURATION_SECS: u64 = 180 * 24 * 60 * 60; // 180 days

    fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now > self.timestamp + Self::CACHE_DURATION_SECS
    }
}

/// Get LAN info using gateway MAC and IP address
/// Returns a readable string ID in format "$gwmac@$gwip", gracefully falls back to default
fn get_lan_info() -> String {
    // Try to get gateway IP and MAC address
    if let Ok(gateway_ip) = get_default_gateway_from_proc() {
        if let Ok(gateway_mac) = get_gateway_mac_from_arp_table(&gateway_ip) {
            // Return in format "$gwmac@$gwip"
            return format!("{mac}@{ip}", mac = gateway_mac.replace(":", "-"), ip = gateway_ip);
        }
    }

    // Fallback to default string if we couldn't get both gateway MAC and IP
    "".to_string()
}

/// Get the MAC address of the gateway by reading /proc/net/arp directly
fn get_gateway_mac_from_arp_table(gateway_ip: &str) -> Result<String> {
    if let Ok(arp_content) = fs::read_to_string("/proc/net/arp") {
        for line in arp_content.lines().skip(1) { // Skip header
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 4 && fields[0] == gateway_ip {
                let mac = fields[3];
                if is_valid_mac_address(mac) {
                    return Ok(mac.replace(":", "-").to_lowercase());
                }
            }
        }
    }

    Err(eyre!("Could not find gateway MAC address for {} in ARP table", gateway_ip))
}

/// Check if a string is a valid MAC address
fn is_valid_mac_address(mac: &str) -> bool {
    if mac.len() != 17 {
        return false;
    }

    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 {
        return false;
    }

    for part in parts {
        if part.len() != 2 {
            return false;
        }
        if !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
    }

    // Check if it's not a broadcast or zero MAC
    mac != "00:00:00:00:00:00" && mac != "ff:ff:ff:ff:ff:ff"
}

/// Get default gateway IP by reading /proc/net/route directly
fn get_default_gateway_from_proc() -> Result<String> {
    if let Ok(contents) = fs::read_to_string("/proc/net/route") {
        for line in contents.lines().skip(1) { // Skip header
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() > 2 && fields[1] == "00000000" { // Default route (destination 0.0.0.0)
                if let Ok(gateway_hex) = u32::from_str_radix(fields[2], 16) {
                    let ip = Ipv4Addr::from(gateway_hex.to_le_bytes());
                    return Ok(ip.to_string());
                }
            }
        }
    }

    Err(eyre!("Could not find default gateway in /proc/net/route"))
}

/// Get cache file path based on LAN hash
fn get_cache_file_path(lan_hash: &str) -> Result<std::path::PathBuf> {
    let cache_dir = dirs().epkg_cache.join("iploc");
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("Failed to create cache directory: {}", cache_dir.display()))?;
    Ok(cache_dir.join(format!("country_code_{}.json", lan_hash)))
}

/// Load country code from cache if valid
fn load_country_code_cache(lan_hash: &str) -> Option<String> {
    let cache_file = get_cache_file_path(lan_hash).ok()?;
    let contents = fs::read_to_string(&cache_file).ok()?;
    let cache: CountryCodeCache = serde_json::from_str(&contents).ok()?;

    // Check if cache is for the same LAN and not expired
    if cache.lan_hash == lan_hash && !cache.is_expired() {
        Some(cache.country_code)
    } else {
        // Clean up expired or invalid cache
        let _ = fs::remove_file(&cache_file);
        None
    }
}

/// Save country code to cache
fn save_country_code_cache(country_code: &str, lan_hash: &str) -> Result<()> {
    let cache_file = get_cache_file_path(lan_hash)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cache = CountryCodeCache {
        country_code: country_code.to_string(),
        timestamp,
        lan_hash: lan_hash.to_string(),
    };

    let json = serde_json::to_string_pretty(&cache)
        .context("Failed to serialize cache data")?;

    // Write to a temp file first
    let pid = process::id();
    let tmp_path = cache_file.with_extension(format!("json.tmp.{}", pid));
    {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create temp cache file: {}", tmp_path.display()))?;
        f.write_all(json.as_bytes())
            .with_context(|| format!("Failed to write to temp cache file: {}", tmp_path.display()))?;
        f.sync_all().ok();
    }

    // Atomically move temp file to final location
    match fs::rename(&tmp_path, &cache_file) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another process won the race, treat as success
            fs::remove_file(&tmp_path)?;
            Ok(())
        }
        Err(e) => {
            // Clean up temp file on error
            let _ = fs::remove_file(&tmp_path);
            Err(e).with_context(|| format!("Failed to atomically move temp cache file to {}", cache_file.display()))
        }
    }
}

pub fn get_country_code_from_ip() -> Result<String> {
    // Get LAN info for caching (gracefully falls back to "default_lan")
    let lan_hash = get_lan_info();

    // Try to load from cache first
    if let Some(cached_country_code) = load_country_code_cache(&lan_hash) {
        return Ok(cached_country_code);
    }

    // Cache miss or expired, fetch from internet with timeout
    let mut resp = ureq::get("http://ipwho.is/")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .build()
        .call()
        .map_err(|e| eyre!("Failed to get country code from ipwho.is: {}", e))?;
    let body = resp.body_mut().read_to_string()
        .map_err(|e| eyre!("Failed to read response body: {}", e))?;
    let ip_info: IpInfo = serde_json::from_str(&body)
        .map_err(|e| eyre!("Failed to parse JSON from ipwho.is: {}", e))?;

    let country_code = ip_info.country_code;

    // Save to cache for future use
    if let Err(e) = save_country_code_cache(&country_code, &lan_hash) {
        log::warn!("Warning: Failed to save country code to cache: {}", e);
    }

    Ok(country_code)
}

// In-memory cache for country code
static COUNTRY_CODE_CACHE: LazyLock<std::sync::Mutex<Option<String>>> = LazyLock::new(|| {
    std::sync::Mutex::new(None)
});

/// Get country code with in-memory caching
pub fn get_country_code() -> Result<String> {
    // Check if we already have a cached value in memory
    if let Ok(cache) = COUNTRY_CODE_CACHE.lock() {
        if let Some(code) = cache.clone() {
            return Ok(code);
        }
    }

    // If not cached, try to get from IP or timezone
    let country_code = get_country_code_from_ip()
        .or_else(|e| {
            log::warn!("Failed to get country from IP, trying timezone. Error: {}", e);
            get_country_code_from_timezone()
        })
        .ok();

    // Cache the result in memory for future calls
    if let Some(code) = &country_code {
        if let Ok(mut cache) = COUNTRY_CODE_CACHE.lock() {
            *cache = Some(code.clone());
        }
    }

    country_code.ok_or_else(|| eyre!("Failed to determine country code"))
}

static TIMEZONE_COUNTRY_MAP: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    load_timezone_country_mapping().unwrap_or_else(|e| {
        eprintln!("Failed to load timezone to country mapping: {}", e);
        HashMap::new()
    })
});

fn load_timezone_country_mapping() -> Result<HashMap<String, String>> {
    let file_path = crate::dirs::get_epkg_src_path().join("sources/cc-timezone.txt");
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let mut map = HashMap::new();
    for line in contents.lines() {
        if let Some((country, tz)) = line.split_once(':') {
            map.insert(tz.trim().to_string(), country.trim().to_string());
        }
    }
    Ok(map)
}

pub fn get_country_code_from_timezone() -> Result<String> {
    let tz_name = get_timezone_name()?;
    TIMEZONE_COUNTRY_MAP.get(&tz_name)
        .cloned()
        .ok_or_else(|| eyre!("Could not map timezone to country code"))
}

fn get_timezone_name() -> Result<String> {
    // Read the /etc/localtime symlink to determine timezone
    let localtime_path = Path::new("/etc/localtime");

    // Check if it's a symlink
    let target = std::fs::read_link(localtime_path)
        .map_err(|e| eyre!("Failed to read /etc/localtime symlink: {}", e))?;

    // Extract timezone from path like "../usr/share/zoneinfo/Asia/Shanghai"
    let target_str = target.to_string_lossy();

    // Find the zoneinfo part in the path
    if let Some(zone_pos) = target_str.find("zoneinfo/") {
        // Extract everything after "zoneinfo/"
        let tz_name = &target_str[(zone_pos + 9)..];
        return Ok(tz_name.to_string());
    }

    Err(eyre!("Could not determine timezone from /etc/localtime symlink"))
}

#[allow(dead_code)]
fn timezone_to_country_code(tz: &str) -> Option<&'static str> {
    TIMEZONE_COUNTRY_MAP.get(tz).map(|s| s.as_str())
}

