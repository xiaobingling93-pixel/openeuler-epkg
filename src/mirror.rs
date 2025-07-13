use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use std::path::Path;
use std::path::PathBuf;
use std::fs;
use time::{OffsetDateTime, UtcOffset};
use time::macros::format_description;
use color_eyre::eyre::{Context, Result, eyre, bail};
use crate::location;
use crate::models::dirs;
use crate::models::channel_config;

const MAX_PGET_LIMIT:usize = 5;

// Add at the top level with other constants
#[allow(dead_code)]
pub const PROTO_HTTP: u8 = 1;   // 0b001
#[allow(dead_code)]
pub const PROTO_HTTPS: u8 = 2;  // 0b010
#[allow(dead_code)]
pub const PROTO_RSYNC: u8 = 4;  // 0b100

// Performance scoring constants
pub const DEFAULT_LATENCY_MS: u32 = 100;                    // Default Mirror latency (when no log available)
pub const DEFAULT_BANDWIDTH_MBPS: u32 = 128;                // Default Mirror total bandwidth (not per-connection throughput)
pub const MIN_THROUGHPUT_BPS: u32 = 1000;                   // Minimum throughput for scoring (1 KB/s)
pub const MAX_THROUGHPUT_BPS: u32 = 10_000_000;             // Maximum throughput for scoring (10 MB/s)
pub const COUNTRY_BONUS_MULTIPLIER: u32 = 8;                // Multiplier for same-country mirrors
pub const MIN_LATENCY_MS: u32 = 10;                         // Minimum latency for scoring
pub const MAX_LATENCY_MS: u32 = 500;                        // Maximum latency for scoring

// Time constants
pub const DAYS_PER_MONTH: i64 = 30;                         // Approximate days per month for log rotation
pub const SECONDS_PER_DAY: u64 = 24 * 3600;                 // Seconds in a day
pub const SECONDS_PER_MONTH: u64 = SECONDS_PER_DAY * DAYS_PER_MONTH as u64; // Approximate seconds per month

// HTTP status code constants
pub const HTTP_FORBIDDEN: u16 = 403;                        // HTTP 403 Forbidden
pub const HTTP_SERVER_ERROR_START: u16 = 500;               // Start of 5xx server errors

// Display and filtering constants
pub const MAX_DISPLAY_MIRRORS: usize = 100;                 // Maximum mirrors to display in stats
pub const DEFAULT_DISPLAY_MIRRORS: usize = 10;              // Default mirrors to display in stats
pub const MIN_MIRRORS_FOR_FILTERING: usize = 3;             // Minimum mirrors needed for performance filtering
pub const RATIO_MIRRORS_FOR_EXPLORATION: usize = 8;         // Power-of-2 divider to explore no-log mirrors at each epkg invocation
pub const ENOUGH_LOCAL_MIRRORS: usize = 10;                 // Enough local mirrors to stop including more world wide ones
pub const INCLUDE_WORLD_MIRRORS: usize = 90;                // Pull in some world wide mirrors on too few local mirrors

// Static variable to track how many times mirror performance stats function was called
static STATS_CALL_COUNT: AtomicU64 = AtomicU64::new(0);

// HTTP event types for non-download operations
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum HttpEvent {
    Latency(u64),               // ms
    NoRange,                    // Server doesn't support range requests
    NetError(String),           // Network error
    HttpStatus(u16),            // HTTP response code
    TooManyRequests(u32),       // Specific event for 429 errors with connection count
    OldContent,                 // Server has old/inconsistent content (for integrity system)
}

// Performance log entry structure (simplified - removed latency_ms, error_type, supports_range, content_available)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PerformanceLog {
    pub timestamp: u64,         // Unix timestamp
    pub url: String,            // The actual URL used for download
    pub offset: u64,            // Starting offset for chunk tasks
    pub bytes_transferred: u64, // Actual bytes transferred from network
    pub duration_ms: u64,       // Total duration including latency
    pub throughput_bps: u64,    // Calculated: bytes_transferred * 1000 / duration_ms
    pub success: bool,          // Whether the operation succeeded
}

// HTTP log entry structure for non-download events
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpLog {
    pub timestamp: u64,      // Unix timestamp
    pub url: String,         // The actual URL used
    pub event: HttpEvent,    // Event type
}

/// Represents a mirror's static configuration.
/// This data is loaded from mirrors.json and is generally not modified at runtime.
#[derive(Debug, Deserialize, Serialize)]
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
    #[serde(skip_serializing, skip_deserializing)]
    pub shared_usage: Arc<SharedUsageStats>, // Mirror usage tracking
    #[serde(skip_serializing, skip_deserializing)]
    pub stats: MirrorStats,
    #[serde(skip_serializing, skip_deserializing)]
    pub is_near: bool,  // true if mirror is in the same country as user
    #[serde(skip_serializing, skip_deserializing)]
    pub skip_urls: std::collections::HashSet<String>, // Skip due to metadata conflicts with master task
}

/// Shared usage statistics that need to be synchronized across Mirror clones
#[derive(Debug, Default)]
pub struct SharedUsageStats {
    pub active_downloads: AtomicUsize,
    pub total_uses: AtomicU64,
    pub last_used: AtomicU64, // Unix timestamp
}

/// Holds all dynamic, statistical, and state-related data for a mirror.
/// This data is loaded from logs and updated during runtime.
#[derive(Debug)]
pub struct MirrorStats {
    pub score: u64,
    pub throughputs: Vec<u32>,  // historical download speeds in bytes/sec
    pub latencies: Vec<u32>,    // historical latencies in milliseconds
    pub avg_throughput: Option<u32>, // cached average throughput for filtering
    pub no_range: bool,         // whether server supports Range requests
    pub no_online: bool,        // whether server is in service
    pub no_content: bool,       // whether server has the files we requested in current run
    pub old_content: bool,      // whether server has old/inconsistent content (integrity system)
    pub max_parallel_conns: Option<u32>, // Learned limit from 429 errors
    pub http_errors: HashMap<u16, u32>,
    pub other_errors: u32,
    pub last_success: Option<u64>,
    pub last_check: Option<u64>,
}

// Manual Default implementation for MirrorStats
impl Default for MirrorStats {
    fn default() -> Self {
        Self {
            score: 0,
            throughputs: Vec::new(),
            latencies: Vec::new(),
            avg_throughput: None,
            no_range: false,
            no_online: false,
            no_content: false,
            old_content: false,
            max_parallel_conns: None,
            http_errors: HashMap::new(),
            other_errors: 0,
            last_success: None,
            last_check: None,
        }
    }
}

// Manual Clone implementation for MirrorStats due to Atomic types.
impl Clone for MirrorStats {
    fn clone(&self) -> Self {
        Self {
            score: self.score,
            throughputs: self.throughputs.clone(),
            latencies: self.latencies.clone(),
            avg_throughput: self.avg_throughput,
            no_range: self.no_range,
            no_online: self.no_online,
            no_content: self.no_content,
            old_content: self.old_content,
            max_parallel_conns: self.max_parallel_conns,
            http_errors: self.http_errors.clone(),
            other_errors: self.other_errors,
            last_success: self.last_success,
            last_check: self.last_check,
        }
    }
}

// Default implementation for Mirror
impl Default for Mirror {
    fn default() -> Self {
        Self {
            url: String::new(),
            distros: Vec::new(),
            distro_dirs: Vec::new(),
            ls_dirs: Vec::new(),
            top_level: false,
            country_code: None,
            protocols: 0,
            bandwidth: None,
            internet2: false,
            shared_usage: Arc::new(SharedUsageStats::default()),
            stats: MirrorStats::default(),
            is_near: false,
            skip_urls: std::collections::HashSet::new(),
        }
    }
}

// Manual Clone implementation for Mirror to properly share usage tracking
impl Clone for Mirror {
    fn clone(&self) -> Self {
        Self {
            url: self.url.clone(),
            distros: self.distros.clone(),
            distro_dirs: self.distro_dirs.clone(),
            ls_dirs: self.ls_dirs.clone(),
            top_level: self.top_level,
            country_code: self.country_code.clone(),
            protocols: self.protocols,
            bandwidth: self.bandwidth,
            internet2: self.internet2,
            shared_usage: Arc::clone(&self.shared_usage), // Share the same usage counters
            stats: self.stats.clone(),
            is_near: self.is_near,
            skip_urls: self.skip_urls.clone(),
        }
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        // Automatically stop usage tracking when Mirror is dropped
        self.stop_usage_tracking();
    }
}

impl Mirror {
    // Helper method to check if a protocol is supported
    #[allow(dead_code)]
    pub fn supports_protocol(&self, protocol: u8) -> bool {
        self.protocols & protocol != 0
    }

    // Helper method to get supported protocols as strings (if needed)
    #[allow(dead_code)]
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
        const MAX_HISTORY: usize = 10;  // Keep last 10 measurements

        // Add new throughput if non-zero
        if throughput > 0 {
            self.stats.throughputs.push(throughput);
            if self.stats.throughputs.len() > MAX_HISTORY {
                self.stats.throughputs.remove(0);
            }
        }

        // Add new latency if non-zero
        if latency > 0 {
            self.stats.latencies.push(latency);
            if self.stats.latencies.len() > MAX_HISTORY {
                self.stats.latencies.remove(0);
            }
        }
    }

    // Helper method to calculate weighted average throughput
    // Newer measurements have higher weights: sum(i * self.throughputs[i]) / sum(i)
    //
    // History data variation could be 10x, so
    // - meaningless using log data for ETA estimation => course 1M beforehand chunking would be suitable
    // - quickly switch to new data for ETA and score calculation => better estimation for ondemand chunking
    //   (but still not good enough to qualify an ETA based global planner)
    //
    //  History data at different time: 10x variation in throughputs!
    //  Rank |  URL                   | Score |  Throughputs (KB/s)     |  Latencies (ms)
    //  -----|------------------------|-------|-------------------------|----------------------
    //    1  | repo.huaweicloud.com   | 13039 | [1294, 945, 1274KB/s]   | [190, 553, 247ms]
    //    2  | repo.huaweicloud.com   |  6783 | [345, 308, 394KB/s]     | [1185, 1154, 1009ms]
    //    2  | repo.huaweicloud.com   | 10985 | [463, 1078, 333KB/s]    | [665, 972, 728ms]
    //    8  | repo.huaweicloud.com   |  3855 | [81, 47, 126KB/s]       | [738, 2287, 1319ms]
    //
    //  History data at same time: throughputs could still have 3-5x variation!
    //  https://mirrors.tuna.tsinghua.edu.cn/ubuntu///dists/noble-updates/by-hash/SHA256/d8c255df1be42d64603734262d5a7833ef4fb8b2a59d0abfb13b093fdb1c6d2d
    //  2025-07-05.16:47:24 offset=10485760 bytes=1048576 dur=3534 tput=303831 ok=1
    //  2025-07-05.16:47:26 offset=11534336 bytes=1048576 dur=1087 tput=987802 ok=1
    //  2025-07-05.16:47:28 offset=13631488 bytes=1048576 dur=1227 tput=875095 ok=1
    //  2025-07-05.16:47:30 offset=14680064 bytes=1048576 dur=1772 tput=605949 ok=1
    //  2025-07-05.16:47:32 offset=19922944 bytes=1048576 dur=895 tput=1199711 ok=1
    //  2025-07-05.16:47:37 offset=20971520 bytes=1048576 dur=3877 tput=276951 ok=1
    //  2025-07-05.16:47:39 offset=22020096 bytes=1048576 dur=1029 tput=1043480 ok=1
    //  2025-07-05.16:47:40 offset=25165824 bytes=1048576 dur=828 tput=1296789 ok=1
    //  2025-07-05.16:47:42 offset=26214400 bytes=1048576 dur=930 tput=1154561 ok=1
    //  2025-07-05.16:47:48 offset=31457280 bytes=1048576 dur=4794 tput=223976 ok=1
    //  2025-07-05.16:47:51 offset=37748736 bytes=1048576 dur=1900 tput=565127 ok=1
    //  2025-07-05.16:47:58 offset=38797312 bytes=1048576 dur=5030 tput=213467 ok=1
    //  2025-07-05.16:48:01 offset=46137344 bytes=1048576 dur=1608 tput=667749 ok=1
    //  2025-07-05.16:48:03 offset=48234496 bytes=1048576 dur=1585 tput=677439 ok=1
    //  2025-07-05.16:48:05 offset=49283072 bytes=1048576 dur=1059 tput=1013920 ok=1
    //  2025-07-05.16:48:10 offset=51380224 bytes=1048576 dur=4856 tput=221116 ok=1
    pub fn avg_throughput(&self) -> Option<u32> {
        if self.stats.throughputs.is_empty() {
            None
        } else {
            // Calculate sum of weights (1 + 2 + ... + n)
            let n = self.stats.throughputs.len();
            let sum_weights = (n * (n + 1)) / 2;

            // Calculate weighted sum: 1*throughputs[0] + 2*throughputs[1] + ... + n*throughputs[n-1]
            let weighted_sum: u64 = self.stats.throughputs.iter().enumerate()
                .map(|(i, &t)| (i + 1) as u64 * t as u64)
                .sum();

            Some((weighted_sum / sum_weights as u64) as u32)
        }
    }

    // Helper method to calculate median latency
    pub fn avg_latency(&self) -> Option<u32> {
        if self.stats.latencies.is_empty() {
            None
        } else {
            // Create a sorted copy of latencies
            let mut sorted = self.stats.latencies.clone();
            sorted.sort();

            // Get the middle value (median)
            let mid = sorted.len() / 2;
            if sorted.len() % 2 == 0 && sorted.len() >= 2 {
                // Even number of elements, average the two middle values
                Some((sorted[mid - 1] + sorted[mid]) / 2)
            } else {
                // Odd number of elements, return the middle value
                Some(sorted[mid])
            }
        }
    }

    /// Start tracking usage for this mirror
    pub fn start_usage_tracking(&mut self) {
        self.shared_usage.active_downloads.fetch_add(1, Ordering::Relaxed);
        self.shared_usage.total_uses.fetch_add(1, Ordering::Relaxed);
        self.shared_usage.last_used.store(
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            Ordering::Relaxed
        );
    }

    /// Stop tracking usage for this mirror
    pub fn stop_usage_tracking(&mut self) {
        // Prevent underflow by checking current value before subtracting
        let current = self.shared_usage.active_downloads.load(Ordering::Relaxed);
        if current > 0 {
            self.shared_usage.active_downloads.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Reset usage tracking counters (useful for debugging corrupted counters)
    #[allow(dead_code)]
    pub fn reset_usage_tracking(&mut self) {
        self.shared_usage.active_downloads.store(0, Ordering::Relaxed);
        self.shared_usage.total_uses.store(0, Ordering::Relaxed);
        self.shared_usage.last_used.store(0, Ordering::Relaxed);
    }

    pub fn add_skip_url(&mut self, url: &str) {
        self.skip_urls.insert(url.to_string());
        log::debug!("Added {} to skip_urls for mirror {} (total skip_urls: {})", url, self.url, self.skip_urls.len());
    }

    pub fn should_skip_url(&self, url: &str) -> bool {
        self.skip_urls.contains(url)
    }

    /// Calculate weighted performance score for mirror selection optimization
    ///
    /// This algorithm combines multiple factors to produce a single score for mirror ranking:
    ///
    /// **Core Performance Metrics:**
    /// - **Throughput**: Based on historical download speeds (higher = better)
    /// - **Latency**: Based on historical response times (lower is better, squared penalty)
    ///
    /// **Fallback Strategy:**
    /// - Uses optimistic defaults for mirrors without historical data to encourage exploration
    /// - Default latency: 30ms, Default throughput: estimated from bandwidth specs
    ///
    /// **Score Formula:**
    /// ```
    /// base_score = avg_throughput / avg_latency
    /// final_score = base_score * country_multiplier
    /// ```
    ///
    /// **Geographic Optimization:**
    /// - 8x bonus for mirrors in the same country as the user
    /// - This heavily favors local mirrors for better performance and compliance
    ///
    /// **Constraints:**
    /// - Throughput capped between 1KB/s and 100MB/s to prevent outliers
    /// - Latency capped between 10ms and 500ms for realistic bounds
    /// - Squared latency penalty amplifies the preference for low-latency mirrors
    ///
    /// **Returns:** Calculated score (higher values indicate better mirrors)
    pub fn calculate_performance_score(&mut self) -> u64 {
        let avg_latency = self.avg_latency().unwrap_or(DEFAULT_LATENCY_MS) as u64;
        let avg_throughput = self.avg_throughput().unwrap_or(
                            self.bandwidth.unwrap_or(DEFAULT_BANDWIDTH_MBPS) * (1024*1024/8/1024)) as u64; // Mbps => B/s; the last /1024 is total_site_bw => my_connection_throughput

        // Store the average throughput for filtering
        self.stats.avg_throughput = Some(avg_throughput as u32);

        // Score based on throughput (higher is better) and latency (lower is better)
        let mut throughput_score = (avg_throughput).min(MAX_THROUGHPUT_BPS as u64).max(MIN_THROUGHPUT_BPS as u64); // Cap in [1KB/s, 10MB/s]

        // Apply country bonus
        // Highly prefer mirrors in same country
        if let Ok(user_country_code) = location::get_country_code() {
            if self.country_code.as_deref() == Some(user_country_code.as_str()) {
                throughput_score *= COUNTRY_BONUS_MULTIPLIER as u64; // Country bonus
            }
        }

        // real tests in CN show there may be 2-3 times difference in same country
        // so divide it twice to prefer the near sites
        let latency_penalty = (avg_latency).min(MAX_LATENCY_MS as u64).max(MIN_LATENCY_MS as u64); // Cap in 10-500 ms
        let performance_score = throughput_score / latency_penalty;
        if performance_score > 0 {
            self.stats.score = performance_score;
        }

        self.stats.score
    }
}

pub struct Mirrors {
    pub mirrors: HashMap<String, Mirror>,   // key: mirror site (without protocol scheme)
    pub available_mirrors: Vec<String>,     // Site names of available mirrors (sorted by score)
    pub pget_limit: usize,                  // Current pget limit for parallel downloads
}

/*
 * ============================================================================
 * STREAMLINED MIRROR MANAGEMENT SYSTEM
 * ============================================================================
 *
 * SIMPLIFIED DESIGN PHILOSOPHY:
 *
 * This system implements country-aware distro-filtered mirror initialization
 * for optimal performance and geographic proximity:
 *
 * 1. **Direct Initialization**: Mirrors are loaded with distro filtering at
 *    startup time using channel_config().distro directly
 *
 * 2. **Country-Based Filtering**: When user country code is available, filters
 *    mirrors to match the user's country for better performance
 *
 * 3. **Smart Fallback Strategy**: If fewer than 3 country-specific mirrors are
 *    found, falls back to all distro mirrors (not all mirrors globally)
 *
 * 4. **Bulk Performance Loading**: All 6 months of performance logs are loaded
 *    at initialization time in a single efficient pass
 *
 * 5. **Integrated Usage Tracking**: Mirror usage is tracked within the Mirrors
 *    struct itself, eliminating the need for separate global state
 *
 * 6. **Date-Based Log Rotation**: Performance logs use monthly rotation with
 *    key=value format for better compatibility and debugging
 *
 * IMPLEMENTATION BENEFITS:
 *
 * - Geographic optimization: Country-based mirror selection when possible
 * - Smart fallback: Ensures adequate mirror availability
 * - Single initialization: No complex re-initialization sequences
 * - Immediate performance data: 6 months of logs loaded at startup
 * - Clean architecture: All mirror state in one place
 * - Future-proof logging: Extensible key=value log format
 */

pub static MIRRORS: LazyLock<Mutex<Mirrors>> = LazyLock::new(|| {
    Mutex::new(Mirrors {
        mirrors: HashMap::new(),
        available_mirrors: Vec::new(),
        pget_limit: 1,
    })
});

/// Merge a single manual mirror into the mirrors collection
fn merge_single_manual_mirror(
    all_mirrors_raw: &mut HashMap<String, Mirror>,
    url: String,
    mut manual_mirror: Mirror,
) {
    if let Some(existing_mirror) = all_mirrors_raw.get_mut(&url) {
        // Merge manual mirror data with existing mirror
        // Manual mirror data takes precedence for key fields
        if !manual_mirror.country_code.is_none() {
            existing_mirror.country_code = manual_mirror.country_code.clone();
        }
        if !manual_mirror.ls_dirs.is_empty() {
            existing_mirror.ls_dirs = manual_mirror.ls_dirs.clone();
        }
        if !manual_mirror.distros.is_empty() {
            existing_mirror.distros = manual_mirror.distros.clone();
        }
        if !manual_mirror.distro_dirs.is_empty() {
            existing_mirror.distro_dirs = manual_mirror.distro_dirs.clone();
        }
        log::trace!("Merged manual mirror data for {}", url);
    } else {
        // Add new manual mirror
        manual_mirror.url = url.clone();
        all_mirrors_raw.insert(url.clone(), manual_mirror);
        log::trace!("Added new manual mirror: {}", url);
    }
}

/// Load and merge manual mirrors into the primary mirrors data
fn load_and_merge_manual_mirrors(
    all_mirrors_raw: &mut HashMap<String, Mirror>,
    manual_mirrors_file_path: &std::path::Path,
) -> Result<()> {
    if manual_mirrors_file_path.exists() {
        log::debug!("Loading manual mirrors from {}", manual_mirrors_file_path.display());

        match fs::read_to_string(manual_mirrors_file_path) {
            Ok(manual_contents) => {
                match serde_json::from_str::<HashMap<String, Mirror>>(&manual_contents) {
                    Ok(manual_mirrors) => {
                        log::debug!("Loaded {} manual mirrors", manual_mirrors.len());

                        // Merge manual mirrors into all_mirrors_raw
                        for (url, manual_mirror) in manual_mirrors {
                            merge_single_manual_mirror(all_mirrors_raw, url, manual_mirror);
                        }
                    },
                    Err(e) => {
                        log::warn!("Failed to parse manual-mirrors.json: {}", e);
                    }
                }
            },
            Err(e) => {
                log::debug!("Could not read manual-mirrors.json: {}", e);
            }
        }
    } else {
        log::debug!("manual-mirrors.json not found, skipping manual mirror loading");
    }

    Ok(())
}

/// Load primary mirrors.json file
fn load_primary_mirrors(mirrors_file_path: &std::path::Path) -> Result<HashMap<String, Mirror>> {
    let contents = fs::read_to_string(mirrors_file_path)
        .with_context(|| format!("Failed to read file: {}", mirrors_file_path.display()))?;

    let all_mirrors_raw: HashMap<String, Mirror> = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse JSON from file: {}", mirrors_file_path.display()))?;

    Ok(all_mirrors_raw)
}

/// Convert URL keys to site keys and merge distros and ls_dirs into distro_dirs
fn convert_mirror_data_structure(all_mirrors_raw: HashMap<String, Mirror>) -> HashMap<String, Mirror> {
    let mut all_mirrors: HashMap<String, Mirror> = HashMap::new();

    for (url, mut mirror) in all_mirrors_raw {
        mirror.distro_dirs.extend(mirror.ls_dirs.clone());
        mirror.distro_dirs.extend(mirror.distros.clone());
        mirror.url = url.clone();

        // Use site name as key instead of full URL
        let site_key = url2site(&url);
        all_mirrors.insert(site_key, mirror);
    }

    all_mirrors
}

/// Apply distro filtering to mirrors if requested
fn apply_distro_filtering(
    all_mirrors: HashMap<String, Mirror>,
    distro_filter: Option<&str>,
) -> Result<HashMap<String, Mirror>> {
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

/// Load channel/mirrors.json with optional distro filtering
///
/// When distro_filter is None, loads all mirrors (used for initial bootstrap)
/// When distro_filter is Some(distro), only loads mirrors supporting that distro
pub fn load_mirrors_for_distro(distro_filter: Option<&str>) -> Result<HashMap<String, Mirror>> {
    let manager_path = crate::dirs::get_epkg_src_path()?;
    let mirrors_file_path = manager_path.join("channel/mirrors.json");
    let manual_mirrors_file_path = manager_path.join("channel/manual-mirrors.json");

    // Load primary mirrors.json
    let mut all_mirrors_raw = load_primary_mirrors(&mirrors_file_path)?;

    // Load and merge manual-mirrors.json if it exists
    load_and_merge_manual_mirrors(&mut all_mirrors_raw, &manual_mirrors_file_path)?;

    // Convert URL keys to site keys and merge distros and ls_dirs into distro_dirs
    let all_mirrors = convert_mirror_data_structure(all_mirrors_raw);

    // Apply distro filtering if requested
    let mut filtered_mirrors = apply_distro_filtering(all_mirrors, distro_filter)?;

    // Initialize performance scores for all mirrors to ensure they have valid stats
    for mirror in filtered_mirrors.values_mut() {
        // Calculate initial performance score if not already set
        if mirror.stats.score == 0 {
            mirror.calculate_performance_score();
        }
    }

    Ok(filtered_mirrors)
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
 *    [1234567890] https://... bytes=1024 dur=500 lat=100 ok=1
 *
 * 5. **Intelligent Mirror Matching**: Each log entry finds its mirror using
 *    URL pattern matching, eliminating the need for per-mirror log loading
 *
 * This approach provides comprehensive performance data immediately at startup
 * while maintaining optimal performance through efficient bulk processing.
 */

/// Append download performance log both to file and in-memory structures
/// This should only be called for actual downloads with bytes > 0
pub fn append_download_log(
    url: &str,
    offset: u64,
    bytes_transferred: u64,
    duration_ms: u64,
    success: bool,
) -> Result<()> {
    // Only log actual downloads with bytes > 0
    if bytes_transferred == 0 {
        return Ok(());
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let throughput_bps = if duration_ms > 0 && bytes_transferred > 0 {
        (bytes_transferred * 1024) / duration_ms
    } else {
        0
    };

    let log_entry = PerformanceLog {
        timestamp,
        url: url.to_string(),
        offset,
        bytes_transferred,
        duration_ms,
        throughput_bps,
        success,
    };

    // Log to file
    append_log_to_file(&log_entry)?;

    // Update in-memory mirror data
    update_mirror_performance(&log_entry)?;

    // Debug output for informative dumps as requested
    if log::log_enabled!(log::Level::Debug) {
        let kbps = throughput_bps / 1024;
        log::debug!(
            "Mirror performance: {} | {} KB/s | {}ms total | {} bytes | offset: {} | success: {}",
            url2site(url),
            kbps,
            duration_ms,
            bytes_transferred,
            offset,
            success,
        );
    }

    Ok(())
}

/// Append HTTP event log for non-download operations
pub fn append_http_log(url: &str, event: HttpEvent) -> Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let http_log = HttpLog {
        timestamp,
        url: url.to_string(),
        event: event.clone(),
    };

    // Log to file
    append_http_log_to_file(&http_log)?;

    // Update in-memory mirror data based on event
    update_mirror_http_event(&http_log)?;

    // Debug output
    if log::log_enabled!(log::Level::Debug) {
        log::debug!(
            "Mirror HTTP event: {} | {:?}",
            url2site(url),
            event,
        );
    }

    Ok(())
}

/// Append log entry to the performance log file with date-based rotation
fn append_log_to_file(log_entry: &PerformanceLog) -> Result<()> {
    use std::io::Write;

    // Generate log file name using proper date formatting
    let log_file_name = generate_log_file_name(log_entry.timestamp);
    let log_file_path = dirs().epkg_downloads_cache.join("log").join(log_file_name);

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
    let log_line = format!("{} {} offset={} bytes={} dur={} tput={} ok={}\n",
        format_timestamp_to_local_datetime(log_entry.timestamp),
        log_entry.url,
        log_entry.offset,
        log_entry.bytes_transferred,
        log_entry.duration_ms,
        log_entry.throughput_bps,
        if log_entry.success { "1" } else { "0" },
    );

    file.write_all(log_line.as_bytes())
        .with_context(|| "Failed to write to log file")?;

    Ok(())
}

/// Append HTTP log entry to the log file with date-based rotation
fn append_http_log_to_file(http_log: &HttpLog) -> Result<()> {
    use std::io::Write;

    // Generate log file name using proper date formatting
    let log_file_name = generate_log_file_name(http_log.timestamp);
    let log_file_path = dirs().epkg_downloads_cache.join("log").join(log_file_name);

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

    // Use key=value format for HTTP events
    let event_str = match &http_log.event {
        HttpEvent::Latency(ms) => format!("latency={}", ms),
        HttpEvent::NoRange => "no_range=1".to_string(),
        HttpEvent::NetError(err) => format!("net_error={}", err),
        HttpEvent::HttpStatus(code) => format!("http_status={}", code),
        HttpEvent::TooManyRequests(count) => format!("too_many_requests={}", count),
        HttpEvent::OldContent => "old_content=1".to_string(),
    };

    let log_line = format!("{} {} {}\n",
        format_timestamp_to_local_datetime(http_log.timestamp),
        http_log.url,
        event_str,
    );

    file.write_all(log_line.as_bytes())
        .with_context(|| "Failed to write HTTP log to file")?;

    Ok(())
}

/// Update in-memory mirror performance data
fn update_mirror_performance(log_entry: &PerformanceLog) -> Result<()> {
    let site = url2site(&log_entry.url);

    if let Ok(mut mirrors_guard) = MIRRORS.lock() {
        if let Some(mirror) = mirrors_guard.mirrors.get_mut(&site) {
            // Update performance data if successful
            if log_entry.success && log_entry.bytes_transferred > 0 {
                mirror.record_performance(
                    log_entry.throughput_bps as u32,
                    0
                );
                mirror.calculate_performance_score();
            }
        }
    }

    Ok(())
}

/*
 * ============================================================================
 * MAX_PARALLEL_CONNS MANAGEMENT FLOW
 * ============================================================================
 *
 * This system implements adaptive connection limiting to prevent HTTP 429
 * "Too Many Requests" errors by learning from server responses and adjusting
 * per-site connection limits accordingly.
 *
 * FLOW OVERVIEW:
 *
 * 1. **Initial State**: All mirrors start with max_parallel_conns = None
 *    (no learned limit, use adaptive_max_concurrent instead)
 *
 * 2. **HTTP 429 Detection**: When a download receives HTTP 429:
 *    - The active connection count is captured from mirror.stats.active_downloads
 *    - HttpEvent::TooManyRequests(conn_count) is logged to file
 *    - update_mirror_http_event() is called immediately
 *
 * 3. **Limit Calculation**: In update_mirror_http_event():
 *    - new_limit = min(conn_count - 1, old_limit)
 *    - This ensures we never exceed the limit that caused 429
 *    - The limit can only decrease, never increase
 *
 * 4. **Persistent Storage**: The limit is saved to log files as:
 *    - Format: "too_many_requests=5" (where 5 is the connection count)
 *    - parse_and_distribute_log_entries() reads this on startup
 *    - Applies the same min(conn_count-1, old_limit) logic
 *
 * 5. **Mirror Selection**: select_best_mirror() respects both limits:
 *    - adaptive_max_concurrent (calculated from performance)
 *    - max_parallel_conns (learned from 429 errors)
 *    - Uses the more restrictive of the two
 *
 * 6. **Automatic Recovery**: The system automatically:
 *    - Skips mirrors that are at their learned limits
 *    - Distributes load to other available mirrors
 *    - Prevents repeated 429 errors from the same mirror
 *
 * BENEFITS:
 * - Prevents cascading 429 errors across multiple downloads
 * - Maintains optimal performance while respecting server limits
 * - Provides persistent learning across application restarts
 * - Enables graceful degradation when servers have strict limits
 *
 * EXAMPLE SCENARIO:
 * - Mirror A has 5 active connections and receives 429
 * - System learns: max_parallel_conns = 4 (5-1)
 * - Future downloads to Mirror A are limited to 4 concurrent connections
 * - If Mirror A receives another 429 with 4 connections, limit becomes 3
 * - System automatically distributes excess load to other mirrors
 */

/// Update in-memory mirror data based on HTTP events
fn update_mirror_http_event(http_log: &HttpLog) -> Result<()> {
    let site = url2site(&http_log.url);

    if let Ok(mut mirrors_guard) = MIRRORS.lock() {
        if let Some(mirror) = mirrors_guard.mirrors.get_mut(&site) {
            let stats = &mut mirror.stats;
            stats.last_check = Some(http_log.timestamp);

            match &http_log.event {
                HttpEvent::Latency(ms) => {
                    stats.latencies.push(*ms as u32);
                },
                HttpEvent::NoRange => {
                    stats.no_range = true;
                },
                HttpEvent::NetError(_) => {
                    stats.no_online = true;
                },
                HttpEvent::HttpStatus(code) => {
                    if *code == 404 {
                        stats.no_content = true;
                    } else if *code == HTTP_FORBIDDEN || *code >= HTTP_SERVER_ERROR_START {
                        stats.no_online = true;
                    }
                    *stats.http_errors.entry(*code).or_insert(0) += 1;
                },
                HttpEvent::TooManyRequests(conn_count_val) => {
                    let conn_count = conn_count_val.clone();
                    // Handle TooManyRequests event: set max_parallel_conns to min(conn_count-1, old_value)
                    let new_limit = if conn_count > 1 { conn_count - 1 } else { 1 };
                    let final_limit = if let Some(old_limit) = stats.max_parallel_conns {
                        new_limit.min(old_limit)
                    } else {
                        new_limit
                    };
                    stats.max_parallel_conns = Some(final_limit);
                    log::debug!("Learned new connection limit for {}: {} (from {} connections) when requesting {}",
                              mirror.url, final_limit, conn_count, http_log.url);
                    // Also record the 429 error in stats
                    *stats.http_errors.entry(429).or_insert(0) += 1;
                },
                HttpEvent::OldContent => {
                    // Mark mirror as having old/inconsistent content for integrity system
                    stats.old_content = true;
                    log::debug!("Mirror {} marked as having old/inconsistent content", mirror.url);
                }
            }
        }
    }

    Ok(())
}

/// Extract the site from a full download URL
pub fn url2site(url: &str) -> String {
    // Normalise to host(:port) -- ignore everything after the first single '/'
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..]; // skip scheme://

        // Take up to the first '/' **ignoring** any extra slashes that may be part of the
        // epkg "///" placeholder syntax. This ensures that URLs such as
        // "https://mirror.example.com/ubuntu///dists/..." are mapped back to the base
        // "https://mirror.example.com" instead of "https://mirror.example.com/ubuntu".
        let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
        return after_scheme[..host_end].to_string(); // Return just the site without scheme
    }

    // Fallback – return unchanged
    url.to_string()
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

    /// Select mirror with automatic usage tracking
    ///
    /// Returns a Mirror that automatically tracks usage when selected and dropped
    pub fn select_mirror_with_usage_tracking(&mut self, need_range: bool, raw_url: Option<&str>) -> Result<Mirror> {
        // Initialize mirrors if not already done
        if self.mirrors.is_empty() {
            let initialized_mirrors = initialize_mirrors()?;

            // Copy the initialized data to self
            self.mirrors = initialized_mirrors.mirrors;
            self.available_mirrors = initialized_mirrors.available_mirrors;
            self.pget_limit = initialized_mirrors.pget_limit;
        }

        let distro = &channel_config().distro;
        let arch = &channel_config().arch;
        let call_count = STATS_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
        let should_dump = call_count % 2 == 1;

        // Update available mirrors based on filtering criteria
        self.update_available_mirrors(need_range, distro, arch, raw_url);

        if self.available_mirrors.is_empty() {
            dump_mirror_performance_stats(&self, true);
            if need_range == true {
                return Err(eyre!("No mirrors with Range support found"));
            } else {
                return Err(eyre!("No available mirrors found in pre-filtered set"));
            }
        }

        if should_dump && log::log_enabled!(log::Level::Debug) {
            dump_mirror_performance_stats(&self, false);
        }

        // Select the best available mirror
        let selected_mirror = self.select_best_mirror()?;
        let mut mirror_clone = selected_mirror.clone();

        // Start usage tracking - since stats is Arc<MirrorStats>, this affects the shared stats
        mirror_clone.start_usage_tracking();

        Ok(mirror_clone)
    }

    /// Filter mirrors based on availability and requirements and update available_mirrors
    fn update_available_mirrors(&mut self, need_range: bool, distro: &str, arch: &str, raw_url: Option<&str>) {
        self.available_mirrors = self.mirrors.iter()
            .filter_map(|(site, mirror)| {
                // Exclude mirrors with no_content, old_content, or no_online
                if mirror.stats.no_content || mirror.stats.old_content || mirror.stats.no_online {
                    return None;
                }

                // If need_range is true, exclude mirrors with no_range=true
                if need_range && mirror.stats.no_range {
                    return None;
                }

                // Exclude mirrors that have metadata conflicts with master task
                if let Some(url) = raw_url {
                    if mirror.should_skip_url(url) {
                        log::trace!("Skipping mirror {} for URL {} due to metadata conflicts", mirror.url, url);
                        return None;
                    }
                }

                // Check if this mirror can provide a valid distro directory
                let distro_dir = Self::find_distro_dir(mirror, distro, arch);
                if distro_dir.is_empty() && mirror.distro_dirs.is_empty() {
                    log::info!("WARNING: Mirror {} does not provide a valid distro directory: {:?}", mirror.url, mirror);
                    return None;
                }

                Some(site.clone())
            })
            .collect();


        // Apply performance-based filtering during initialization
        self.filter_mirrors_by_performance();

        // Sort available mirrors by score (descending)
        self.available_mirrors.sort_by(|a, b| {
            let score_a = self.mirrors.get(a).map(|m| m.stats.score).unwrap_or(0);
            let score_b = self.mirrors.get(b).map(|m| m.stats.score).unwrap_or(0);
            score_b.cmp(&score_a)
        });
    }

    /*
     * PERFORMANCE-BASED MIRROR FILTERING
     *
     * This filter implements performance-based mirror selection by:
     *
     * 1. Collecting mirrors with throughput data (logs)
     * 2. Calculating the median throughput (mid_throughput)
     * 3. Filtering out mirrors whose avg_throughput < mid_throughput / (1 + MAX_PGET_LIMIT - pget_limit)
     *
     * Since pget_limit grows over time, we gradually filter out more and more slow mirrors.
     * - The newly collected throughput data can be more accuracte than history data;
     * - The more closer to the end, the more important we select high speed mirrors,
     *   because the overall ETA is determined by the slowest task
     */
    fn filter_mirrors_by_performance(&mut self) {
        // Get mirrors with throughput data
        let mirrors_with_throughputs: Vec<&str> = self.available_mirrors.iter()
            .filter(|site| {
                self.mirrors.get(*site)
                    .map(|mirror| mirror.stats.avg_throughput.is_some())
                    .unwrap_or(false)
            })
            .map(|site| site.as_str())
            .collect();

        // If we don't have enough mirrors with throughput data, skip filtering
        if mirrors_with_throughputs.len() < MIN_MIRRORS_FOR_FILTERING {
            return;
        }

        // Calculate median throughput (mid_throughput)
        let mut throughputs: Vec<u32> = mirrors_with_throughputs.iter()
            .filter_map(|site| {
                self.mirrors.get(*site)
                    .and_then(|mirror| mirror.stats.avg_throughput)
            })
            .collect();

        throughputs.sort();
        let mid_throughput = throughputs[throughputs.len() / 2];

        // Calculate threshold based on current pget_limit
        // Ensure we never divide by zero – self.pget_limit can temporarily exceed MAX_PGET_LIMIT
        let denom_raw = if self.pget_limit <= MAX_PGET_LIMIT {
            1 + MAX_PGET_LIMIT as u32 - self.pget_limit as u32
        } else {
            1 // When pget_limit exceeds MAX_PGET_LIMIT, use minimum denominator
        };
        let denom = std::cmp::max(denom_raw, 1); // clamp to >=1
        let threshold = if denom > 0 {
            mid_throughput / denom
        } else {
            mid_throughput
        };

        // Filter out mirrors below the threshold
        self.available_mirrors.retain(|site| {
            if let Some(mirror) = self.mirrors.get(site) {
                if let Some(avg_throughput) = mirror.stats.avg_throughput {
                    // Keep mirrors with throughput above threshold
                    avg_throughput >= threshold
                } else {
                    // Keep mirrors without throughput data (for exploration)
                    true
                }
            } else {
                true
            }
        });
    }

    /// Select the best available mirror using adaptive connection limiting
    ///
    /// This function implements an intelligent mirror selection strategy that balances
    /// load distribution with performance optimization:
    ///
    /// **Adaptive Connection Limiting:**
    /// - Starts with `pget_limit=1` to give all mirrors equal opportunity
    /// - Gradually increases the limit as parallel tasks grow
    /// - Fast mirrors complete downloads sooner and become available for reuse
    /// - This creates a natural preference for high-performing mirrors
    ///
    /// **Load Distribution:**
    /// - Mirrors are selected based on current connection usage vs. limits
    /// - Respects both adaptive limits and learned limits from HTTP 429 errors
    /// - Non-local mirrors get half the connection limit of local mirrors
    /// - Prevents overloading any single mirror
    ///
    /// **Performance Optimization:**
    /// - Mirrors are pre-sorted by performance score (highest first)
    /// - Fast mirrors naturally receive more traffic due to faster completion
    /// - Slower mirrors at the end of the list get fewer opportunities
    /// - Falls back to round-robin selection if all limits are exceeded
    fn select_best_mirror(&mut self) -> Result<&Mirror> {
        if self.available_mirrors.is_empty() {
            return Err(eyre!("No mirrors with valid distro directories found"));
        }

        // Find the minimum limit that has available mirrors
        let mut current_limit = self.pget_limit;
        let (selected_site, successful_limit) = loop {
            if let Some(site) = self.find_first_site_under_thresh(current_limit) {
                break (site.to_string(), current_limit);
            }

            // If no mirrors found under threshold, increment limit and try again
            current_limit += 1;

            // Prevent infinite loop - if we've tried beyond reasonable limits,
            // just return the highest scoring mirror
            if current_limit > MAX_PGET_LIMIT {
                log::warn!("WARNING: pget_limit exceeded {}, selecting highest scoring mirror", MAX_PGET_LIMIT);
                let call_count = STATS_CALL_COUNT.load(Ordering::Relaxed);
                let rand_site = self.available_mirrors[call_count as usize % self.available_mirrors.len()].clone();
                break (rand_site, current_limit);
            }
        };

        // Update pget_limit after all borrowing is done
        self.pget_limit = successful_limit;

        // Now get the mirror reference safely
        let mirror = self.mirrors.get(&selected_site).unwrap();
        let current_usage = mirror.shared_usage.active_downloads.load(Ordering::Relaxed);
        let learned_limit = mirror.stats.max_parallel_conns;

        log::debug!("Selected mirror: {} (usage: {}/{} learned limit: {:?} pget_limit: {})",
            mirror.url,
            current_usage,
            mirror.stats.max_parallel_conns.unwrap_or(0),
            learned_limit,
            self.pget_limit
        );

        Ok(mirror)
    }

    /// Find the first site under the given connection limit threshold
    fn find_first_site_under_thresh(&self, limit: usize) -> Option<&str> {
        for site in &self.available_mirrors {
            if let Some(mirror) = self.mirrors.get(site) {
                let current_usage = mirror.shared_usage.active_downloads.load(Ordering::Relaxed);
                let effective_limit = calculate_effective_limit(mirror, limit);

                // Check if this mirror is under the effective limit
                if current_usage < effective_limit {
                    return Some(site);
                }
            }
        }
        None
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
            if let Some(orig_dir) = mirror.distro_dirs.iter().find(|dir| dir.eq_ignore_ascii_case(item)) {
                // Use the original casing from the mirror itself to avoid wrong capitalisation
                found_dir = orig_dir.clone();
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



    /// Get current usage statistics for debugging
    #[allow(dead_code)]
    pub fn get_usage_stats(&self) -> HashMap<String, (usize, u64, u64)> {
        self.mirrors.iter()
            .map(|(url, mirror)| {
                let active = mirror.shared_usage.active_downloads.load(Ordering::Relaxed);
                let total = mirror.shared_usage.total_uses.load(Ordering::Relaxed);
                let last_used = mirror.shared_usage.last_used.load(Ordering::Relaxed);
                (url.clone(), (active, total, last_used))
            })
            .collect()
    }

    /// Reset all mirror usage tracking counters (useful for debugging corrupted counters)
    #[allow(dead_code)]
    pub fn reset_all_usage_tracking(&mut self) {
        for mirror in self.mirrors.values_mut() {
            mirror.reset_usage_tracking();
        }
        log::info!("Reset usage tracking counters for all mirrors");
    }

}


/// Calculate effective connection limit for a mirror based on pget_limit and whether it's near
fn calculate_effective_limit(mirror: &Mirror, pget_limit: usize) -> usize {
    // Base limit:
    // - For *near* (same-country) mirrors we allow the full current `pget_limit`.
    // - For *remote* mirrors we intentionally start with **half** the limit.
    //   When `pget_limit == 1` that becomes **0**, which deliberately excludes
    //   remote mirrors from the very first selection scan.  As `pget_limit`
    //   rises with traffic pressure those mirrors will gradually become
    //   eligible (0 -> 1 -> 2 ...).
    let base_limit = if mirror.is_near {
        pget_limit
    } else {
        pget_limit / 2
    };

    // Honour any learned per-mirror limit (from previous 429 errors); if none
    // exists use the base limit calculated above.  A value of 0 means the
    // mirror is currently not eligible, so cannot apply min() on it.
    if let Some(per_mirror_limit) = mirror.stats.max_parallel_conns {
        std::cmp::min(per_mirror_limit as usize, base_limit)
    } else {
        base_limit
    }
}

/// Show performance stats for a single mirror
fn show_one_mirror(rank: usize, site: &str, mirror: &Mirror, pget_limit: usize) {
    // Show last 3 throughputs and latencies
    let recent_throughputs: Vec<u32> = mirror.stats.throughputs.iter().rev().take(3).copied().collect();
    let recent_latencies: Vec<u32> = mirror.stats.latencies.iter().rev().take(3).copied().collect();

    let throughput_str = if recent_throughputs.is_empty() {
        "[none]".to_string()
    } else {
        format!("[{}KB/s]", recent_throughputs.iter()
            .map(|&t| format!("{}", t / 1024))
            .collect::<Vec<_>>()
            .join(", "))
    };

    let latency_str = if recent_latencies.is_empty() {
        "[none]".to_string()
    } else {
        format!("[{}ms]", recent_latencies.iter()
            .map(|&l| format!("{}", l))
            .collect::<Vec<_>>()
            .join(", "))
    };

    // Build status flags string - removed is_available check
    let mut status_flags: Vec<String> = Vec::new();
    if mirror.stats.no_range {
        status_flags.push("NoRange".to_string());
    }
    if mirror.stats.no_content {
        status_flags.push("NoContent".to_string());
    }
    if mirror.stats.old_content {
        status_flags.push("OldContent".to_string());
    }
    if mirror.stats.no_online {
        status_flags.push("NoOnline".to_string());
    }
    // Show learned maximum parallel connections if present
    if let Some(limit) = mirror.stats.max_parallel_conns {
        status_flags.push(format!("Limit={}", limit));
    }
    // Show country code for non-near sites
    if !mirror.is_near {
        if let Some(ref country_code) = mirror.country_code {
            status_flags.push(format!("cc={}", country_code));
        }
    }
    let status_str = status_flags.join(", ");

    println!(
        " {:2}  | {:38} | {:5} | {:2}<{:2}<{:2} | {:32} | {:28} | {}",
        rank,
        // Truncate long URLs for alignment
        if site.len() > 38 { site[..35].to_string() + "..." } else { site.to_string() },
        mirror.stats.score,
        mirror.shared_usage.active_downloads.load(Ordering::Relaxed),
        calculate_effective_limit(mirror, pget_limit),
        mirror.shared_usage.total_uses.load(Ordering::Relaxed),
        // Limit throughput string length for alignment
        if throughput_str.len() > 28 { throughput_str[..26].to_string() + ".." } else { throughput_str },
        latency_str,
        status_str,
    );
}

/// Debug function to dump mirror performance stats with unavailable mirrors
pub fn dump_mirror_performance_stats(mirrors: &Mirrors, show_all: bool) {
    println!("=== Top Mirrors with Stats ===");
    println!("");
    println!("Rank |  URL                                   | Score |  Usage   |  Throughputs (KB/s)              |  Latencies (ms)              | Status Flags");
    println!("-----|----------------------------------------|-------|----------|----------------------------------|------------------------------|---------------------------");

    let max_avail = if show_all { MAX_DISPLAY_MIRRORS } else { DEFAULT_DISPLAY_MIRRORS };

    // Show available mirrors first
    for (i, site) in mirrors.available_mirrors.iter().enumerate().take(max_avail) {
        if let Some(mirror) = mirrors.mirrors.get(site) {
            show_one_mirror(i + 1, site, mirror, mirrors.pget_limit);
        }
    }

    if show_all {
        println!("");
        println!("=== Unavailable Mirrors ===");

        // Show mirrors not in available_mirrors
        let mut unavailable_count = 0;
        for (site, mirror) in &mirrors.mirrors {
            if !mirrors.available_mirrors.contains(site) {
                unavailable_count += 1;
                show_one_mirror(unavailable_count, site, mirror, mirrors.pget_limit);
            }
        }

        if unavailable_count == 0 {
            println!("No unavailable mirrors found.");
        }
    }

    println!("");
    println!("=== End Mirror Stats ===");
}

/// EU country codes list
const EU_COUNTRY_CODES: &[&str] = &[
    "AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR",
    "DE", "GR", "HU", "IE", "IT", "LV", "LT", "LU", "MT", "NL",
    "PL", "PT", "RO", "SK", "SI", "ES", "SE"
];

/// Check if a country code is in the EU
fn is_eu_country(country_code: &str) -> bool {
    EU_COUNTRY_CODES.contains(&country_code)
}

/// Apply EU-specific filtering logic
fn apply_eu_filtering(mut local_mirrors: HashMap<String, Mirror>, eu_mirrors: HashMap<String, Mirror>) -> HashMap<String, Mirror> {
    // For EU countries, always use local_mirrors + eu_mirrors
    log::debug!("EU country detected - using local mirrors ({}) + EU mirrors ({})",
                local_mirrors.len(), eu_mirrors.len());

    local_mirrors.extend(eu_mirrors);
    local_mirrors
}

/// Apply general fallback logic for non-EU countries
fn apply_general_fallback(mut local_mirrors: HashMap<String, Mirror>, other_mirrors: HashMap<String, Mirror>) -> HashMap<String, Mirror> {
    let local_count = local_mirrors.len();

    if local_count >= ENOUGH_LOCAL_MIRRORS {
        // Use only local mirrors if we have enough
        log::debug!("Using only local mirrors ({} mirrors, should be sufficient)", local_count);
        local_mirrors
    } else {
        // Take up to INCLUDE_WORLD_MIRRORS from other_mirrors to supplement local mirrors
        let needed_from_other = INCLUDE_WORLD_MIRRORS.min(other_mirrors.len());

        // Sort other_mirrors by score (descending) before selecting
        let mut sorted_other: Vec<_> = other_mirrors.into_iter().collect();
        sorted_other.sort_by(|(_, a), (_, b)| b.stats.score.cmp(&a.stats.score));

        let mut selected_other = HashMap::new();

        // Take the top mirrors by score
        for (site, mirror) in sorted_other.into_iter().take(needed_from_other) {
            selected_other.insert(site, mirror);
        }

        log::debug!("Using local mirrors ({}) + {} world wide mirrors",
                    local_count, selected_other.len());

        local_mirrors.extend(selected_other);
        local_mirrors
    }
}

/// Apply country code filtering to mirrors
fn apply_country_code_filtering(mirrors: HashMap<String, Mirror>) -> HashMap<String, Mirror> {
    // Apply country code filtering if we have a user country code
    // Don't block on country code detection - proceed if it fails or times out
    match location::get_country_code() {
        Ok(user_country_code) => {
            log::debug!("Filtering mirrors by country code: {}", user_country_code);

            // Create separate HashMaps for different mirror categories
            let mut local_mirrors = HashMap::new();
            let mut eu_mirrors = HashMap::new();
            let mut other_mirrors = HashMap::new();

            for (site, mut mirror) in mirrors {
                if mirror.country_code.as_deref() == Some(user_country_code.as_str()) {
                    // Local country mirrors
                    mirror.is_near = true;
                    local_mirrors.insert(site, mirror);
                } else if let Some(ref country_code) = mirror.country_code {
                    if is_eu_country(country_code) {
                        // EU mirrors (but not local)
                        mirror.is_near = false;
                        eu_mirrors.insert(site, mirror);
                    } else {
                        // Other non-EU mirrors
                        mirror.is_near = false;
                        other_mirrors.insert(site, mirror);
                    }
                } else {
                    // Mirrors without country code
                    mirror.is_near = false;
                    other_mirrors.insert(site, mirror);
                }
            }

            log::debug!("Found {} local mirrors, {} EU mirrors, {} other mirrors for country {}",
                       local_mirrors.len(), eu_mirrors.len(), other_mirrors.len(), user_country_code);

            // Apply appropriate filtering strategy
            if is_eu_country(&user_country_code) {
                apply_eu_filtering(local_mirrors, eu_mirrors)
            } else {
                apply_general_fallback(local_mirrors, other_mirrors)
            }
        }
        Err(e) => {
            log::debug!("Failed to get country code: {}, using all distro mirrors", e);
            // Set all mirrors as not near
            mirrors.into_iter()
                .map(|(site, mut mirror)| {
                    mirror.is_near = false;
                    (site, mirror)
                })
                .collect()
        }
    }
}

/// Filter mirrors by exploration criteria (removes mirrors without throughput data)
/*
 * MIRROR EXPLORATION FILTERING
 *
 * This filter implements adaptive exploration of new mirrors based on how many
 * mirrors already have performance history (throughput data). The goal is to:
 *
 * 1. Prioritize mirrors with known performance data for reliability
 * 2. Allow controlled exploration of new mirrors without performance history
 * 3. Reduce exploration as more mirrors with data become available
 *
 * This ensures we gradually shift from exploration to exploitation as we
 * gather more mirror performance data over time.
 */
fn filter_mirrors_by_exploration(mirrors: &mut HashMap<String, Mirror>) {
    let nr_has_log = mirrors.values()
        .filter(|mirror| !mirror.stats.throughputs.is_empty())
        .count();

    let max_empty_throughputs = if nr_has_log > RATIO_MIRRORS_FOR_EXPLORATION + 1 {
        // slow exploration when we have plenty of known sites
        nr_has_log / RATIO_MIRRORS_FOR_EXPLORATION
    } else if nr_has_log <= 3 {
        // skip filter by empty throughputs (aggressive exploration when we lack data)
        mirrors.len()
    } else {
        RATIO_MIRRORS_FOR_EXPLORATION + 2 - nr_has_log
    };

    if max_empty_throughputs < mirrors.len() {
        let mut empty_count = 0;
        mirrors.retain(|_, mirror| {
            if mirror.stats.throughputs.is_empty() {
                empty_count += 1;
                if empty_count > max_empty_throughputs {
                    return false;  // Filter out this mirror
                }
            }
            true  // Keep this mirror
        });
    }
}

/// Initialize mirrors with distro and country code filtering
fn initialize_mirrors() -> Result<Mirrors> {
    let mirrors = match load_mirrors_for_distro(Some(&channel_config().distro)) {
        Ok(m) if !m.is_empty() => apply_country_code_filtering(m),
        Ok(_) | Err(_) => {
            bail!("Failed to load mirrors for distro '{}'", channel_config().distro);
        }
    };

    // Load performance logs efficiently - process all log files at once
    let mut loaded_mirrors = mirrors;
    load_performance_logs(&mut loaded_mirrors);

    // Calculate performance scores for all mirrors after loading logs
    for mirror in loaded_mirrors.values_mut() {
        mirror.calculate_performance_score();
    }

    // Explore some new sites in each epkg invocation
    filter_mirrors_by_exploration(&mut loaded_mirrors);

    let mirrors = Mirrors {
        mirrors: loaded_mirrors,
        available_mirrors: Vec::new(), // Will be populated by update_available_mirrors() when needed
        pget_limit: 1,
    };

    if log::log_enabled!(log::Level::Debug) {
        dump_mirror_performance_stats(&mirrors, true);
    }
    Ok(mirrors)
}

/// Load performance logs from recent log files at once
fn load_performance_logs(mirrors: &mut HashMap<String, Mirror>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Generate the last 6 months (including current month) using proper date formatting
    let months_to_check = generate_recent_month_strings(now, 6);

    for month in months_to_check {
        let log_file_path = dirs().epkg_downloads_cache
            .join(format!("log/mirror-{}.log", month));

        if log_file_path.exists() {
            if let Err(e) = parse_and_distribute_log_entries(&log_file_path, mirrors) {
                log::debug!("Failed to parse log file {}: {}", log_file_path.display(), e);
            }
        }
    }
}

/// Parse log file and distribute entries to appropriate mirrors
fn parse_and_distribute_log_entries(
    log_file_path: &std::path::Path,
    mirrors: &mut HashMap<String, Mirror>,
) -> Result<()> {
    let contents = fs::read_to_string(log_file_path)?;

    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let mut bytes_transferred = 0u64;
        let mut throughput_bps = 0u64;
        let mut success = false;
        let mut latency_ms = None;
        let mut no_range = None;
        let mut net_error = None;
        let mut http_status = None;
        let mut too_many_requests: Option<u32> = None;
        let mut old_content = None;

        // Split the line into tokens
        let tokens: Vec<&str> = line.split_whitespace().collect();

        if tokens.len() < 2 {
            continue;
        }

        // Format: time url key=value key=value...
        let url = tokens[1].to_string();

        // Parse the remaining key=value pairs
        for &token in tokens.iter() {
            if let Some((key, value)) = token.split_once('=') {
                match key {
                    "bytes" => bytes_transferred = value.parse().unwrap_or(0),
                    "tput" => throughput_bps = value.parse().unwrap_or(0),
                    "ok" => success = value == "1" || value == "true",
                    "lat" | "latency" => latency_ms = Some(value.parse().unwrap_or(0)),
                    "no_range" => no_range = Some(value == "1" || value == "true"),
                    "net_error" => net_error = Some(value.to_string()),
                    "http_status" => http_status = Some(value.parse().unwrap_or(0)),
                    "too_many_requests" => too_many_requests = value.parse().ok(),
                    "old_content" => old_content = Some(value == "1" || value == "true"),
                    "dur" => {}, // Duration field, ignored for now
                    _ => {} // Ignore unknown keys for forward compatibility
                }
            }
        }

        // Find the mirror this log entry belongs to and update its online status
        //
        // Mirror online status logic:
        // 1. Mark mirror as online (no_online = false) on any successful download with throughput data
        // 2. Mark mirror as offline (no_online = true) only when:
        //    - Network/HTTP errors occur AND no historical throughput data exists
        //    - This prevents mirrors with past performance data from being incorrectly marked offline
        //      due to temporary network issues or isolated errors
        //
        // Example of mirrors that should remain available despite errors:
        // Rank |  URL                                   | Score |  Usage   |  Throughputs (KB/s)              |  Latencies (ms)              | Status Flags
        // -----|----------------------------------------|-------|----------|----------------------------------|------------------------------|---------------------------
        // === Unavailable Mirrors ===
        //   1  | mirrors.zju.edu.cn                     |  3345 |  0< 0< 0 | [101, 178, 147KB/s]              | [1404, 1513, 1135ms]         | NoOnline
        //   2  | mirrors.ustc.edu.cn                    |  1746 |  0< 0< 0 | [109, 61, 31KB/s]                | [2733, 363, 2485ms]          | NoOnline
        //
        // Problem in a corner case: if a site happen to be offline at first access, then the log
        // file will have http error and no throughput data, that site will be excluded until the
        // log file expired after months.
        let site = url2site(&url);
        if let Some(mirror) = mirrors.get_mut(&site) {
            // Update mirror attributes based on the log entry
            if bytes_transferred > 0 && success {
                // This is a download log entry
                mirror.record_performance(throughput_bps as u32, 0);
                mirror.calculate_performance_score();
                mirror.stats.no_online = false;
            } else if let Some(latency) = latency_ms {
                // This is a latency event
                mirror.record_performance(0, latency as u32);
                mirror.calculate_performance_score();
            } else if let Some(true) = no_range {
                // Server doesn't support range requests (permanent attribute)
                mirror.stats.no_range = true;
            } else if net_error.is_some() {
                if mirror.stats.throughputs.is_empty() {
                    mirror.stats.no_online = true;
                }
            } else if let Some(code) = http_status {
                if code == HTTP_FORBIDDEN || code >= HTTP_SERVER_ERROR_START {
                    if mirror.stats.throughputs.is_empty() {
                        mirror.stats.no_online = true;
                    }
                    // DO NOT set no_content on 404 history logs: it may well be temp
                    // issue due to rsync delays, or error in a different distro/version
                }
            } else if let Some(conn_count_val) = too_many_requests {
                let conn_count = conn_count_val;
                // Handle TooManyRequests event: set max_parallel_conns to min(conn_count-1, old_value)
                let new_limit = if conn_count > 1 { conn_count - 1 } else { 1 };
                let final_limit = if let Some(old_limit) = mirror.stats.max_parallel_conns {
                    new_limit.min(old_limit)
                } else {
                    new_limit
                };
                mirror.stats.max_parallel_conns = Some(final_limit);
                log::trace!("Learned new connection limit for {}: {} (from {} connections) when requesting {}",
                          mirror.url, final_limit, conn_count, url);
                // Also record the 429 error in stats
                *mirror.stats.http_errors.entry(429).or_insert(0) += 1;
            } else if let Some(true) = old_content {
                // Handle OldContent event: mark mirror as having old/inconsistent content
                mirror.stats.old_content = true;
                log::trace!("Mirror {} marked as having old/inconsistent content (from log)", mirror.url);
            }
        }
    }

    Ok(())
}

/// Convert Unix timestamp to formatted datetime string using time crate
fn format_timestamp_to_local_datetime(timestamp: u64) -> String {
    // Convert timestamp to OffsetDateTime in UTC first
    match OffsetDateTime::from_unix_timestamp(timestamp as i64) {
        Ok(utc_datetime) => {
            // Try to get local offset and convert to local time
            let local_datetime = if let Ok(local_offset) = UtcOffset::current_local_offset() {
                utc_datetime.to_offset(local_offset)
            } else {
                // Fallback to UTC if we can't get local offset (e.g., in multi-threaded environment)
                utc_datetime
            };

            // Use the same format as in history.rs for consistency
            local_datetime.format(&format_description!("[year]-[month]-[day].[hour repr:24]:[minute]:[second]"))
                .unwrap_or_else(|_| format!("{}", timestamp))
        },
        Err(_) => format!("{}", timestamp), // Fallback to timestamp if conversion fails
    }
}

/// Generate log file name from timestamp using proper date formatting
fn generate_log_file_name(timestamp: u64) -> String {
    match OffsetDateTime::from_unix_timestamp(timestamp as i64) {
        Ok(utc_datetime) => {
            // Try to get local offset and convert to local time, fallback to UTC
            let datetime = if let Ok(local_offset) = UtcOffset::current_local_offset() {
                utc_datetime.to_offset(local_offset)
            } else {
                // Fallback to UTC if we can't get local offset
                utc_datetime
            };

            // Use YYYY-MM format for monthly log rotation
            datetime.format(&format_description!("[year]-[month padding:zero]"))
                .map(|date_str| format!("mirror-{}.log", date_str))
                .unwrap_or_else(|_| {
                    // Final fallback - use UTC formatting directly
                    utc_datetime.format(&format_description!("[year]-[month padding:zero]"))
                        .map(|date_str| format!("mirror-{}.log", date_str))
                        .unwrap_or_else(|_| format!("mirror-{}.log", timestamp))
                })
        },
        Err(_) => {
            // Last resort fallback if timestamp conversion completely fails
            format!("mirror-{}.log", timestamp)
        }
    }
}

/// Generate a list of month strings for the last N months using proper date formatting
fn generate_recent_month_strings(timestamp: u64, months_back: usize) -> Vec<String> {
    match OffsetDateTime::from_unix_timestamp(timestamp as i64) {
        Ok(utc_datetime) => {
            // Try to get local offset and convert to local time, fallback to UTC
            let current_datetime = if let Ok(local_offset) = UtcOffset::current_local_offset() {
                utc_datetime.to_offset(local_offset)
            } else {
                // Fallback to UTC if we can't get local offset
                utc_datetime
            };

            let mut month_strings = Vec::new();

            for i in 0..months_back {
                // Calculate the date for i months ago (approximate with 30 days per month)
                let days_to_subtract = i as i64 * DAYS_PER_MONTH;
                let target_date = if let Some(target) = current_datetime.checked_sub(time::Duration::days(days_to_subtract)) {
                    target
                } else {
                    // Try UTC fallback if local time calculation fails
                    if let Some(target) = utc_datetime.checked_sub(time::Duration::days(days_to_subtract)) {
                        target
                    } else {
                        continue;
                    }
                };

                if let Ok(month_str) = target_date.format(&format_description!("[year]-[month padding:zero]")) {
                    month_strings.push(month_str);
                }
            }

            month_strings
        },
        Err(_) => {
            // Last resort fallback if timestamp conversion completely fails
            vec![format!("{}", timestamp / SECONDS_PER_MONTH)] // Very rough month approximation
        }
    }
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
