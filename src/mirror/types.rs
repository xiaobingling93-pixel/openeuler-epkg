//! # Mirror Data Structures and Constants
//!
//! This module defines the core data structures and constants used throughout the mirror
//! management system. It provides the foundation for mirror configuration, performance
//! tracking, and runtime state management.
//!
//! ## Key Data Structures
//!
//! - **Mirror**: Static mirror configuration loaded from mirrors.json
//! - **MirrorStats**: Dynamic performance and state data updated during runtime
//! - **SharedUsageStats**: Thread-safe usage tracking shared across mirror clones
//! - **Mirrors**: Container for all mirror data with global state management
//! - **PerformanceLog/HttpLog**: Structured logging entries for analytics
//!
//! ## Constants
//!
//! - **Performance thresholds**: Throughput and latency bounds for scoring
//! - **Geographic bonuses**: Country-based performance multipliers
//! - **Connection limits**: Parallel download and exploration constraints
//! - **Time constants**: Log rotation and data retention periods
//! - **HTTP status codes**: Error handling and status classification
//!
//! ## Thread Safety
//!
//! All shared state uses appropriate synchronization primitives:
//! - `Arc<SharedUsageStats>` for cross-thread usage tracking
//! - `LazyLock<Mutex<Mirrors>>` for global mirror state
//! - Atomic counters for statistics and call tracking

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::sync::atomic::{AtomicU64, AtomicUsize};


pub const MAX_PGET_LIMIT:usize = 5;

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

// NoOnline threshold constants
pub const MIN_ATTEMPTS_FOR_NOONLINE: usize = 2;             // Minimum attempts before marking as NoOnline
pub const MAX_NOONLINE_FRACTION_DENOM: usize = 3;           // Maximum fraction of mirrors that can be NoOnline: 1/MAX_NOONLINE_FRACTION_DENOM

// Static variable to track how many times mirror performance stats function was called
pub static STATS_CALL_COUNT: AtomicU64 = AtomicU64::new(0);

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
    pub distros: HashSet<String>,
    #[serde(rename = "dir")]
    #[serde(default)]
    pub distro_dirs: HashSet<String>,
    #[serde(rename = "pdir")]
    #[serde(default)]
    pub probe_dirs: HashSet<String>,    // will be merged into distro_dirs after JSON loading
    #[serde(rename = "ls")]
    #[serde(default)]
    pub ls_dirs: HashSet<String>,       // will be merged into distro_dirs after JSON loading
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
    pub skip_urls: HashSet<String>, // Skip due to metadata conflicts with master task
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
    pub no_content: u32,        // counter: how many times server returned 404 for files we requested
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
            no_content: 0,
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
            distros: HashSet::new(),
            distro_dirs: HashSet::new(),
            probe_dirs: HashSet::new(),
            ls_dirs: HashSet::new(),
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
            probe_dirs: self.distro_dirs.clone(),
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
