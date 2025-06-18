use serde::{Deserialize, Serialize};
use crate::models::channel_config;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, Arc};
use std::path::Path;
use std::path::PathBuf;
use crate::dirs::get_epkg_manager_path;
use crate::models::dirs;
use color_eyre::eyre::{Context, Result, eyre};
use std::fs;
use crate::location;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
// Removed chrono dependency

// Add at the top level with other constants
pub const PROTO_HTTP: u8 = 1;   // 0b001
pub const PROTO_HTTPS: u8 = 2;  // 0b010
pub const PROTO_RSYNC: u8 = 4;  // 0b100

// Performance log entry structure
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PerformanceLog {
    pub timestamp: u64,      // Unix timestamp
    pub url: String,         // The actual URL used for download
    pub bytes_transferred: u64, // Actual bytes transferred from network
    pub duration_ms: u64,    // Total duration including latency
    pub latency_ms: u64,     // Just the initial connection/request latency
    pub throughput_bps: u64, // Calculated: bytes_transferred * 1000 / duration_ms
    pub success: bool,       // Whether the operation succeeded
    pub error_type: Option<String>, // HTTP error code or error description
    pub supports_range: Option<bool>, // Whether server supports Range requests
    pub content_available: Option<bool>, // Whether content was available (not 404)
}

// Mirror usage tracking
#[derive(Debug, Default)]
pub struct MirrorUsage {
    pub active_downloads: AtomicUsize,
    pub total_uses: AtomicU64,
    pub last_used: AtomicU64, // Unix timestamp
}

// Mirror configuration with compact field names
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mirror {
    #[serde(default)]
    #[serde(skip_serializing)]
    pub url: String,
    #[serde(rename = "os")]
    #[serde(default)]
    pub distros: Vec<String>,
    #[serde(rename = "dir")]
    #[serde(default)]
    pub distro_dirs: Vec<String>,
    #[serde(rename = "ls")]
    #[serde(default)]
    pub ls_dirs: Vec<String>,   // will be merged into distro_dirs after JSON loading
    #[serde(rename = "root", default, deserialize_with = "crate::mirror::bool_from_number")]
    pub top_level: bool,
    #[serde(rename = "cc")]
    #[serde(default)]
    pub country_code: Option<String>,
    #[serde(rename = "p", default)]
    pub protocols: u8,  //  PROTO_HTTP | PROTO_HTTPS | PROTO_RSYNC
    #[serde(rename = "bw")]
    #[serde(default)]
    pub bandwidth: Option<u32>,
    #[serde(rename = "i2", default, deserialize_with = "crate::mirror::bool_from_number")]
    pub internet2: bool,
    #[serde(default)]
    pub score: u32,
    #[serde(default)]
    pub throughputs: Vec<u32>,  // historical download speeds in bytes/sec
    #[serde(default)]
    pub latencies: Vec<u32>,    // historical latencies in milliseconds
    #[serde(default)]
    pub no_range: bool,         // whether server supports Range requests
    #[serde(default)]
    pub no_online: bool,        // whether server is in service
    #[serde(default)]
    pub no_content: bool,       // whether server has the files we requested
    #[serde(default)]
    pub performance_logs: Vec<PerformanceLog>, // Recent performance history
}

impl Mirror {
    // Helper method to check if a protocol is supported
    pub fn supports_protocol(&self, protocol: u8) -> bool {
        self.protocols & protocol != 0
    }

    // Helper method to get supported protocols as strings (if needed)
    pub fn protocol_list(&self) -> Vec<String> {
        let mut protocols = Vec::new();
        if self.supports_protocol(PROTO_HTTP) {
            protocols.push("http".to_string());
        }
        if self.supports_protocol(PROTO_HTTPS) {
            protocols.push("https".to_string());
        }
        if self.supports_protocol(PROTO_RSYNC) {
            protocols.push("rsync".to_string());
        }
        protocols
    }

    // Helper method to update performance metrics
    pub fn record_performance(&mut self, throughput: u32, latency: u32) {
        const MAX_HISTORY: usize = 5;  // Keep last 5 measurements

        // Add new throughput
        self.throughputs.push(throughput);
        if self.throughputs.len() > MAX_HISTORY {
            self.throughputs.remove(0);
        }

        // Add new latency
        self.latencies.push(latency);
        if self.latencies.len() > MAX_HISTORY {
            self.latencies.remove(0);
        }
    }

    // Helper method to calculate average throughput
    pub fn avg_throughput(&self) -> Option<u32> {
        if self.throughputs.is_empty() {
            None
        } else {
            Some(self.throughputs.iter().sum::<u32>() / self.throughputs.len() as u32)
        }
    }

    // Helper method to calculate average latency
    pub fn avg_latency(&self) -> Option<u32> {
        if self.latencies.is_empty() {
            None
        } else {
            Some(self.latencies.iter().sum::<u32>() / self.latencies.len() as u32)
        }
    }

    // Calculate weighted performance score based on recent history
    pub fn calculate_performance_score(&self) -> f64 {
        if self.performance_logs.is_empty() {
            return self.score as f64;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut weighted_score = 0.0;
        let mut weight_sum = 0.0;

        // Filter to recent logs (last 24 hours) and successful transfers
        let recent_logs: Vec<_> = self.performance_logs.iter()
            .filter(|log| {
                let age_hours = (now - log.timestamp) / 3600;
                age_hours <= 24 && log.success && log.bytes_transferred > 0
            })
            .collect();

        if recent_logs.is_empty() {
            return self.score as f64;
        }

        for log in recent_logs {
            let age_hours = (now - log.timestamp) / 3600;
            let recency_weight = 1.0 / (1.0 + age_hours as f64 * 0.1); // Newer logs have higher weight

            // Score based on throughput (higher is better) and latency (lower is better)
            let throughput_score = (log.throughput_bps as f64 / 1_000_000.0).min(100.0); // Cap at 100 Mbps
            let latency_penalty = (log.latency_ms as f64 / 1000.0).min(5.0); // Cap penalty at 5 seconds
            let performance_score = throughput_score - latency_penalty;

            weighted_score += performance_score * recency_weight;
            weight_sum += recency_weight;
        }

        if weight_sum > 0.0 {
            let final_score = (weighted_score / weight_sum) + self.score as f64;
            final_score.max(0.0)
        } else {
            self.score as f64
        }
    }

    // Add a new performance log entry
    pub fn add_performance_log(&mut self, log: PerformanceLog) {
        const MAX_LOGS: usize = 50; // Keep last 50 performance logs

        self.performance_logs.push(log);

        // Keep only recent logs
        if self.performance_logs.len() > MAX_LOGS {
            self.performance_logs.remove(0);
        }

        // Also update the legacy throughputs/latencies arrays for backward compatibility
        if let Some(last_log) = self.performance_logs.last() {
            if last_log.success && last_log.bytes_transferred > 0 {
                self.record_performance(
                    last_log.throughput_bps as u32,
                    last_log.latency_ms as u32
                );
            }
        }
    }

    // Check if mirror has recent failed attempts
    pub fn has_recent_failures(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.performance_logs.iter()
            .filter(|log| (now - log.timestamp) < 3600) // Last hour
            .any(|log| !log.success)
    }

    // Get recent success rate (0.0 to 1.0)
    pub fn recent_success_rate(&self) -> f64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let recent_logs: Vec<_> = self.performance_logs.iter()
            .filter(|log| (now - log.timestamp) < 3600) // Last hour
            .collect();

        if recent_logs.is_empty() {
            return 1.0; // Default to good if no recent data
        }

        let success_count = recent_logs.iter().filter(|log| log.success).count();
        success_count as f64 / recent_logs.len() as f64
    }
}

pub struct Mirrors {
    pub mirrors: HashMap<String, Mirror>, // key: mirror url
    pub mirror_usage: HashMap<String, Arc<MirrorUsage>>, // Track concurrent usage
}

/*
 * ============================================================================
 * STREAMLINED MIRROR MANAGEMENT SYSTEM
 * ============================================================================
 *
 * SIMPLIFIED DESIGN PHILOSOPHY:
 *
 * This system implements direct distro-filtered mirror initialization for
 * optimal performance and simplicity:
 *
 * 1. **Direct Initialization**: Mirrors are loaded with distro filtering at
 *    startup time using channel_config().distro directly
 *
 * 2. **Fallback Strategy**: If distro-specific loading fails or returns empty,
 *    automatically falls back to loading all mirrors
 *
 * 3. **Bulk Performance Loading**: All 6 months of performance logs are loaded
 *    at initialization time in a single efficient pass
 *
 * 4. **Integrated Usage Tracking**: Mirror usage is tracked within the Mirrors
 *    struct itself, eliminating the need for separate global state
 *
 * 5. **Date-Based Log Rotation**: Performance logs use monthly rotation with
 *    key=value format for better compatibility and debugging
 *
 * IMPLEMENTATION BENEFITS:
 *
 * - Single initialization: No complex re-initialization sequences
 * - Immediate performance data: 6 months of logs loaded at startup
 * - Clean architecture: All mirror state in one place
 * - Future-proof logging: Extensible key=value log format
 */

pub static MIRRORS: LazyLock<Mutex<Mirrors>> = LazyLock::new(initialize_mirrors);

/// Load channel/mirrors.json with optional distro filtering
///
/// When distro_filter is None, loads all mirrors (used for initial bootstrap)
/// When distro_filter is Some(distro), only loads mirrors supporting that distro
pub fn load_mirrors_for_distro(distro_filter: Option<&str>) -> Result<HashMap<String, Mirror>> {
    let file_path = get_epkg_manager_path()?.join("channel/mirrors.json");
    let contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;

    let mut all_mirrors: HashMap<String, Mirror> = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", file_path.display()))?;

    // Merge ls_dirs into distro_dirs and assign URL keys for all mirrors
    for (url, mirror) in all_mirrors.iter_mut() {
        mirror.distro_dirs.extend(mirror.ls_dirs.clone());
        mirror.url = url.clone();
    }

    // Apply distro filtering if requested
    if let Some(target_distro) = distro_filter {
        let original_count = all_mirrors.len();
        let filtered_mirrors: HashMap<String, Mirror> = all_mirrors
            .into_iter()
            .filter(|(_, mirror)| {
                // Check if mirror is suitable for the target distro
                is_mirror_suitable_for_distro(mirror, target_distro)
            })
            .collect();

        log::debug!(
            "Filtered mirrors for distro '{}': {} out of {} mirrors selected",
            target_distro,
            filtered_mirrors.len(),
            original_count
        );

        Ok(filtered_mirrors)
    } else {
        log::debug!("Loading all mirrors without distro filtering");
        Ok(all_mirrors)
    }
}

/// Check if a mirror is suitable for the given distro, considering architecture-specific rules
fn is_mirror_suitable_for_distro(mirror: &Mirror, target_distro: &str) -> bool {
    // Basic distro support check
    if !mirror.distros.contains(&target_distro.to_string()) {
        return false;
    }

    let distro = &channel_config().distro;
    let arch = &channel_config().arch;

    // Fedora-specific architecture rules
    if distro == "fedora" {
        if arch != "x86_64" && arch != "aarch64" {
            // For non-primary architectures, mirror must support secondary repos
            return mirror.distro_dirs.iter().any(|dir| dir.contains("secondary"));
        }
    }

    // Ubuntu-specific architecture rules
    if distro == "ubuntu" {
        if arch != "x86_64" {
            // For non-x86_64 architectures, mirror must support ports
            return mirror.distro_dirs.iter().any(|dir| dir.contains("ports"));
        }
    }

    true
}

/*
 * ============================================================================
 * OPTIMIZED PERFORMANCE TRACKING SYSTEM
 * ============================================================================
 *
 * EFFICIENT BULK LOADING STRATEGY:
 *
 * Performance logs are now loaded using an optimized bulk processing approach:
 *
 * 1. **6-Month Historical Window**: Loads performance data from the last 6 months
 *    instead of just 2 months for better performance insights
 *
 * 2. **Single-Pass Processing**: All log files are processed once, with each log
 *    entry automatically distributed to its corresponding mirror
 *
 * 3. **Date-Based Rotation**: Log files use YYYY-MM format for automatic monthly
 *    rotation (e.g., mirror-2024-03.log)
 *
 * 4. **Key=Value Format**: Future-proof logging format that supports easy
 *    extension without breaking compatibility:
 *    ts=1234567890 url=https://... bytes=1024 dur=500 lat=100 ok=1
 *
 * 5. **Intelligent Mirror Matching**: Each log entry finds its mirror using
 *    URL pattern matching, eliminating the need for per-mirror log loading
 *
 * This approach provides comprehensive performance data immediately at startup
 * while maintaining optimal performance through efficient bulk processing.
 */

/// Append download performance log both to file and in-memory structures
pub fn append_download_log(
    url: &str,
    bytes_transferred: u64,
    duration_ms: u64,
    latency_ms: u64,
    success: bool,
    error_type: Option<String>,
    supports_range: Option<bool>,
    content_available: Option<bool>,
) -> Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let throughput_bps = if duration_ms > 0 && bytes_transferred > 0 {
        (bytes_transferred * 1000) / duration_ms
    } else {
        0
    };

    let log_entry = PerformanceLog {
        timestamp,
        url: url.to_string(),
        bytes_transferred,
        duration_ms,
        latency_ms,
        throughput_bps,
        success,
        error_type: error_type.clone(),
        supports_range,
        content_available,
    };

    // Log to file
    append_log_to_file(&log_entry)?;

    // Update in-memory mirror data
    update_mirror_performance(&log_entry)?;

    // Debug output for informative dumps as requested
    if log::log_enabled!(log::Level::Debug) {
        let mbps = throughput_bps as f64 / 1_000_000.0;
        log::debug!(
            "Mirror performance: {} | {:.2} MB/s | {}ms latency | {}ms total | {} bytes | success: {}{}{}",
            extract_mirror_base_url(url),
            mbps,
            latency_ms,
            duration_ms,
            bytes_transferred,
            success,
            error_type.as_ref().map(|e| format!(" | error: {}", e)).unwrap_or_default(),
            if let Some(range) = supports_range {
                format!(" | range: {}", range)
            } else {
                String::new()
            }
        );
    }

    Ok(())
}

/// Append log entry to the performance log file with date-based rotation
fn append_log_to_file(log_entry: &PerformanceLog) -> Result<()> {
    use std::io::Write;

    // Generate current month string using standard library
    let days_since_epoch = log_entry.timestamp / (24 * 3600);
    let current_year = 1970 + (days_since_epoch / 365);
    let current_month = ((days_since_epoch % 365) / 30) + 1;

    let log_file_name = format!("mirror-{}-{:02}.log", current_year, current_month);
    let log_file_path = dirs().epkg_downloads_cache.join("logs").join(log_file_name);

    // Ensure parent directory exists
    if let Some(parent) = log_file_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create log directory: {}", parent.display()))?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .with_context(|| format!("Failed to open log file: {}", log_file_path.display()))?;

    // Use key=value format for better compatibility and extensibility
    let log_line = format!("ts={} url={} bytes={} dur={} lat={} tput={} ok={} err={} range={} avail={}\n",
        log_entry.timestamp,
        log_entry.url,
        log_entry.bytes_transferred,
        log_entry.duration_ms,
        log_entry.latency_ms,
        log_entry.throughput_bps,
        if log_entry.success { "1" } else { "0" },
        log_entry.error_type.as_deref().unwrap_or("-"),
        log_entry.supports_range.map(|b| if b { "1" } else { "0" }).unwrap_or("-"),
        log_entry.content_available.map(|b| if b { "1" } else { "0" }).unwrap_or("-"),
    );

    file.write_all(log_line.as_bytes())
        .with_context(|| "Failed to write to log file")?;

    Ok(())
}

/// Update in-memory mirror performance data
fn update_mirror_performance(log_entry: &PerformanceLog) -> Result<()> {
    let mirror_base_url = extract_mirror_base_url(&log_entry.url);

    if let Ok(mut mirrors_guard) = MIRRORS.lock() {
        if let Some(mirror) = mirrors_guard.mirrors.get_mut(&mirror_base_url) {
            mirror.add_performance_log(log_entry.clone());

            // Update mirror status flags based on the log
            if let Some(supports_range) = log_entry.supports_range {
                mirror.no_range = !supports_range;
            }

            if let Some(content_available) = log_entry.content_available {
                mirror.no_content = !content_available;
                mirror.no_online = !content_available && !log_entry.success;
            }
        }
    }

    Ok(())
}

/// Extract the base mirror URL from a full download URL
fn extract_mirror_base_url(url: &str) -> String {
    // Handle URLs with triple slashes (our mirror format)
    if let Some(triple_slash_pos) = url.find("///") {
        url[..triple_slash_pos].to_string()
    } else if let Some(scheme_end) = url.find("://") {
        // For regular URLs, find the domain part
        let after_scheme = &url[scheme_end + 3..];
        if let Some(path_start) = after_scheme.find('/') {
            format!("{}://{}", &url[..scheme_end], &after_scheme[..path_start])
        } else {
            url.to_string()
        }
    } else {
        url.to_string()
    }
}

/*
 * ============================================================================
 * INTELLIGENT MIRROR SELECTION SYSTEM
 * ============================================================================
 *
 * STREAMLINED SELECTION ALGORITHM:
 *
 * Mirror selection operates on pre-filtered mirrors with comprehensive performance
 * data, enabling sophisticated selection based on:
 *
 * 1. **Rich Historical Data**: 6 months of performance logs loaded at startup
 * 2. **Geographic Proximity**: Country-based optimization when available
 * 3. **Load Balancing**: Concurrent usage limits with weighted distribution
 * 4. **Failure Recovery**: Automatic fallback based on recent success rates
 *
 * WEIGHTED DISTRIBUTION BENEFITS:
 *
 * Uses probabilistic selection rather than always picking the highest-scoring
 * mirror, providing natural load balancing:
 *
 * - High-performing mirrors get proportionally more traffic
 * - Secondary mirrors remain active for diversity and resilience
 * - Concurrent limits automatically distribute load across mirrors
 *
 * PRE-FILTERED EFFICIENCY:
 *
 * Since mirrors are distro-filtered at initialization:
 * - No runtime distro checking required
 * - Simplified selection logic
 * - Faster mirror selection during downloads
 * - All available mirrors are guaranteed compatible
 */

impl Mirrors {

    /// Select mirror with usage tracking and concurrent limits
    ///
    /// This is the core mirror selection algorithm that balances performance,
    /// availability, and load distribution, with distro directory validation
    pub fn select_mirror_with_usage_tracking(&self, max_concurrent: usize) -> Result<Mirror> {
        let distro = &channel_config().distro;
        let arch = &channel_config().arch;

        // Since mirrors are pre-filtered by distro, we only need to check basic availability
        let available_mirrors: Vec<&Mirror> = self.mirrors.values()
            .filter(|m| !m.no_online && !m.no_content)
            .collect();

        if available_mirrors.is_empty() {
            return Err(eyre!("No available mirrors found in pre-filtered set"));
        }

        let user_country_code = location::get_country_code().ok();

        if let Some(cc) = &user_country_code {
            eprintln!("Detected country: {}", cc);
        }

        // Calculate scores for all available mirrors and validate distro directories
        let mut valid_mirrors: Vec<(Mirror, f64, usize)> = Vec::new();

        for mirror in available_mirrors {
            // Check if this mirror can provide a valid distro directory
            let distro_dir = Self::find_distro_dir(mirror, distro, arch);
            if distro_dir.is_empty() && mirror.distro_dirs.is_empty() {
                // Skip mirrors that have no valid distro directories
                continue;
            }

            let mut score = mirror.calculate_performance_score();

            // Apply country bonus
            if let Some(user_cc) = &user_country_code {
                if mirror.country_code.as_deref() == Some(user_cc.as_str()) {
                    score *= 2.0; // Country bonus
                }
            }

            // Apply success rate multiplier
            score *= mirror.recent_success_rate();

            // Penalty for recent failures
            if mirror.has_recent_failures() {
                score *= 0.5;
            }

            // Get current usage
            let current_usage = self.mirror_usage.get(&mirror.url)
                .map(|usage| usage.active_downloads.load(Ordering::Relaxed))
                .unwrap_or(0);

            valid_mirrors.push((mirror.clone(), score, current_usage));
        }

        if valid_mirrors.is_empty() {
            return Err(eyre!("No mirrors with valid distro directories found"));
        }

        // Sort by score descending
        valid_mirrors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Filter out mirrors that are at their concurrent limit
        let available_mirrors: Vec<_> = valid_mirrors.iter()
            .filter(|(_, _, usage)| *usage < max_concurrent)
            .collect();

        if available_mirrors.is_empty() {
            // All mirrors are at capacity, return the highest scoring one anyway
            return Ok(valid_mirrors[0].0.clone());
        }

        // Weighted selection based on scores
        let total_score: f64 = available_mirrors.iter().map(|(_, score, _)| score).sum();

        if total_score <= 0.0 {
            // Fallback to first available mirror
            return Ok(available_mirrors[0].0.clone());
        }

        // Select mirror with probability proportional to score
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        std::thread::current().id().hash(&mut hasher);
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().hash(&mut hasher);
        let random_value = (hasher.finish() % 10000) as f64 / 10000.0; // 0.0 to 1.0

        let mut cumulative_score = 0.0;
        for (mirror, score, _) in &available_mirrors {
            cumulative_score += score / total_score;
            if random_value <= cumulative_score {
                return Ok(mirror.clone());
            }
        }

        // Fallback
        Ok(available_mirrors[0].0.clone())
    }

    /// Find the best matching distro directory for a mirror
    pub fn find_distro_dir(mirror: &Mirror, distro: &str, arch: &str) -> String {
        // Use pre-sorted distro_dirs from channel config (sorted during deserialization)
        let sorted_dirs = &channel_config().distro_dirs;

        let mut found_dir = String::new();
        for item in sorted_dirs {
            let item_lower = item.to_lowercase();
            if distro == "fedora" {
                if item_lower.contains("alt") {
                    continue;
                }
                if item_lower.contains("archive") {
                    continue;
                }
                if arch == "x86_64" || arch == "aarch64" {
                    if item_lower.contains("secondary") {
                        continue;
                    }
                } else {
                    if !item_lower.contains("secondary") {
                        continue;
                    }
                }
            }
            if distro == "ubuntu" {
                if arch == "x86_64" {
                    if item_lower.contains("ports") {
                        continue;
                    }
                } else {
                    if !item_lower.contains("ports") {
                        continue;
                    }
                }
            }
            if mirror.distro_dirs.iter().any(|dir| dir.to_lowercase() == item_lower) {
                found_dir = item.clone();
                break;
            }
        }
        found_dir
    }

    /// Format mirror URL based on mirror configuration and package format
    ///
    /// Parameters provided directly to avoid dependency on Mirror struct
    pub fn format_mirror_url(&self, mirror_url: &str, top_level: bool, distro_dir: &str) -> Result<String> {
        let distro = &channel_config().distro;

        // Debian's index_url has explicit "$mirror/debian/", "$mirror/debian-security/"
        let url = if top_level || distro == "debian" {
                      format!("{}//", mirror_url.trim_end_matches('/'))
                  } else {
                      format!("{}/{}//", mirror_url.trim_end_matches('/'), distro_dir)
                  };

        Ok(url)
    }

    pub fn url_to_cache_path(url: &str) -> Result<PathBuf> {
        let cache_root = dirs().epkg_downloads_cache.clone();
        Ok(Self::resolve_mirror_path(url, &cache_root))
    }

    pub fn resolve_mirror_path(url: &str, output_dir: &Path) -> PathBuf {
        let final_path = if let Some((_, str_b)) = url.split_once("$mirror/") {
            let distro = &channel_config().distro;
            let arch = &channel_config().arch;
            let mut local_subdir = String::new();

            if distro == "fedora" {
                if arch != "x86_64" && arch != "aarch64" {
                    local_subdir = "fedora-secondary".to_string();
                }
            }

            if distro == "ubuntu" {
                if arch != "x86_64" {
                    local_subdir = "ubuntu-ports".to_string();
                }
            }

            if local_subdir.is_empty() && distro != "debian" {
                local_subdir = distro.clone();
            }

            output_dir.join(&local_subdir).join(str_b)
        } else if let Some((_, str_b)) = url.split_once("///") {
            output_dir.join(str_b)
        } else {
            let file_name = url.split('/').last()
                .unwrap_or("unknown_file");
            output_dir.join(file_name)
        };

        final_path
    }

    /// Increment active usage counter for a mirror
    pub fn increment_mirror_usage(&self, mirror_url: &str) {
        let base_url = extract_mirror_base_url(mirror_url);
        if let Some(usage) = self.mirror_usage.get(&base_url) {
            usage.active_downloads.fetch_add(1, Ordering::Relaxed);
            usage.total_uses.fetch_add(1, Ordering::Relaxed);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            usage.last_used.store(now, Ordering::Relaxed);
        }
    }

    /// Decrement active usage counter for a mirror
    pub fn decrement_mirror_usage(&self, mirror_url: &str) {
        let base_url = extract_mirror_base_url(mirror_url);
        if let Some(usage) = self.mirror_usage.get(&base_url) {
            usage.active_downloads.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Get current usage statistics for debugging
    pub fn get_usage_stats(&self) -> HashMap<String, (usize, u64, u64)> {
        self.mirror_usage.iter()
            .map(|(url, usage)| {
                let active = usage.active_downloads.load(Ordering::Relaxed);
                let total = usage.total_uses.load(Ordering::Relaxed);
                let last_used = usage.last_used.load(Ordering::Relaxed);
                (url.clone(), (active, total, last_used))
            })
            .collect()
    }
}

/// Helper function to track mirror usage (called from download code)
pub fn track_mirror_usage_start(url: &str) {
    if let Ok(mirrors) = MIRRORS.lock() {
        mirrors.increment_mirror_usage(url);
    }
}

/// Helper function to track mirror usage end (called from download code)
pub fn track_mirror_usage_end(url: &str) {
    if let Ok(mirrors) = MIRRORS.lock() {
        mirrors.decrement_mirror_usage(url);
    }
}

/// Debug function to dump mirror performance stats
pub fn dump_mirror_performance_stats() {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }

    if let Ok(mirrors) = MIRRORS.lock() {
        log::debug!("=== Mirror Performance Stats ===");

        let mut sorted_mirrors: Vec<_> = mirrors.mirrors.iter().collect();
        sorted_mirrors.sort_by(|a, b| {
            b.1.calculate_performance_score()
                .partial_cmp(&a.1.calculate_performance_score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for (url, mirror) in sorted_mirrors.iter().take(10) {
            let score = mirror.calculate_performance_score();
            let success_rate = mirror.recent_success_rate();
            let usage_stats = mirrors.mirror_usage.get(*url)
                .map(|u| (
                    u.active_downloads.load(Ordering::Relaxed),
                    u.total_uses.load(Ordering::Relaxed)
                ))
                .unwrap_or((0, 0));

            log::debug!(
                "Mirror: {} | Score: {:.1} | Success: {:.1}% | Usage: {}/{} | Logs: {}",
                url,
                score,
                success_rate * 100.0,
                usage_stats.0,
                usage_stats.1,
                mirror.performance_logs.len()
            );

            // Show recent performance summary
            if !mirror.performance_logs.is_empty() {
                let recent_logs: Vec<_> = mirror.performance_logs.iter()
                    .rev()
                    .take(3)
                    .collect();

                for log in recent_logs {
                    let mbps = log.throughput_bps as f64 / 1_000_000.0;
                    log::debug!(
                        "  Recent: {:.1} MB/s | {}ms | {} bytes | {}",
                        mbps,
                        log.latency_ms,
                        log.bytes_transferred,
                        if log.success { "OK" } else { "FAIL" }
                    );
                }
            }
        }
        log::debug!("=== End Mirror Stats ===");
    }
}

/// Initialize mirrors with distro filtering and load performance logs
fn initialize_mirrors() -> Mutex<Mirrors> {
    let mirrors = match load_mirrors_for_distro(Some(&channel_config().distro)) {
        Ok(m) if !m.is_empty() => m,
        Ok(_) | Err(_) => {
            // Either got empty mirrors for the distro or failed to load, try fallback
            eprintln!("Failed to load mirrors for distro '{}', trying all mirrors", channel_config().distro);
            match load_mirrors_for_distro(None) {
                Ok(fallback) => fallback,
                Err(e2) => {
                    eprintln!("Failed to load any mirrors: {}", e2);
                    HashMap::new()
                }
            }
        }
    };

    // Load performance logs efficiently - process all log files at once
    let mut loaded_mirrors = mirrors;
    load_all_performance_logs(&mut loaded_mirrors);

    let mut mirror_usage = HashMap::new();
    for url in loaded_mirrors.keys() {
        mirror_usage.insert(url.clone(), Arc::new(MirrorUsage::default()));
    }

    Mutex::new(Mirrors { mirrors: loaded_mirrors, mirror_usage })
}

/// Load performance logs from all available log files at once
fn load_all_performance_logs(mirrors: &mut HashMap<String, Mirror>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Generate current and previous 6 months strings using standard library
    let days_since_epoch = now / (24 * 3600);
    let current_year = 1970 + (days_since_epoch / 365);
    let current_month = ((days_since_epoch % 365) / 30) + 1;

    // Generate the last 6 months (including current month)
    let mut months_to_check = Vec::new();
    for i in 0..6 {
        let months_back = i;
        let (year, month) = if current_month > months_back {
            (current_year, current_month - months_back)
        } else {
            // Need to go back to previous year
            let months_needed = months_back - current_month + 1;
            let years_back = (months_needed / 12) + 1;
            let month_in_prev_year = 12 - (months_needed % 12);
            (current_year - years_back, if month_in_prev_year == 12 { 12 } else { month_in_prev_year })
        };
        months_to_check.push(format!("{}-{:02}", year, month));
    }

    // Load logs from the last 6 months (180 days)
    let cutoff_time = now.saturating_sub(180 * 24 * 3600);

    for month in months_to_check {
        let log_file_path = dirs().epkg_downloads_cache
            .join(format!("logs/mirror-{}.log", month));

        if log_file_path.exists() {
            if let Err(e) = parse_and_distribute_log_entries(&log_file_path, mirrors, cutoff_time) {
                log::debug!("Failed to parse log file {}: {}", log_file_path.display(), e);
            }
        }
    }
}

/// Parse log file and distribute entries to appropriate mirrors
fn parse_and_distribute_log_entries(
    log_file_path: &std::path::Path,
    mirrors: &mut HashMap<String, Mirror>,
    cutoff_time: u64
) -> Result<()> {
    let contents = fs::read_to_string(log_file_path)?;

    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Parse key=value format
        let mut log_entry = PerformanceLog {
            timestamp: 0,
            url: String::new(),
            bytes_transferred: 0,
            duration_ms: 0,
            latency_ms: 0,
            throughput_bps: 0,
            success: false,
            error_type: None,
            supports_range: None,
            content_available: None,
        };

        for pair in line.split_whitespace() {
            if let Some((key, value)) = pair.split_once('=') {
                match key {
                    "ts" => log_entry.timestamp = value.parse().unwrap_or(0),
                    "url" => log_entry.url = value.to_string(),
                    "bytes" => log_entry.bytes_transferred = value.parse().unwrap_or(0),
                    "dur" => log_entry.duration_ms = value.parse().unwrap_or(0),
                    "lat" => log_entry.latency_ms = value.parse().unwrap_or(0),
                    "tput" => log_entry.throughput_bps = value.parse().unwrap_or(0),
                    "ok" => log_entry.success = value == "1" || value == "true",
                    "err" => if !value.is_empty() && value != "-" {
                        log_entry.error_type = Some(value.to_string());
                    },
                    "range" => if !value.is_empty() && value != "-" {
                        log_entry.supports_range = Some(value == "1" || value == "true");
                    },
                    "avail" => if !value.is_empty() && value != "-" {
                        log_entry.content_available = Some(value == "1" || value == "true");
                    },
                    _ => {} // Ignore unknown keys for forward compatibility
                }
            }
        }

        // Skip old entries
        if log_entry.timestamp < cutoff_time {
            continue;
        }

        // Find the mirror this log entry belongs to
        let mirror_base_url = extract_mirror_base_url(&log_entry.url);
        if let Some(mirror) = mirrors.get_mut(&mirror_base_url) {
            mirror.add_performance_log(log_entry);
        }
    }

    Ok(())
}

// Helper to deserialize bools that may be represented as 0/1 numbers in JSON
pub fn bool_from_number<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Visitor};
    use std::fmt;

    struct BoolVisitor;

    impl<'de> Visitor<'de> for BoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a boolean or 0/1")
        }

        fn visit_bool<E>(self, v: bool) -> Result<bool, E> {
            Ok(v)
        }

        fn visit_u64<E>(self, v: u64) -> Result<bool, E>
        where
            E: Error,
        {
            Ok(v != 0)
        }

        fn visit_i64<E>(self, v: i64) -> Result<bool, E>
        where
            E: Error,
        {
            Ok(v != 0)
        }

        fn visit_str<E>(self, v: &str) -> Result<bool, E>
        where
            E: Error,
        {
            match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "y" => Ok(true),
                "0" | "false" | "no" | "n" => Ok(false),
                _ => Err(E::custom(format!("invalid bool value: {}", v))),
            }
        }
    }

    deserializer.deserialize_any(BoolVisitor)
}

