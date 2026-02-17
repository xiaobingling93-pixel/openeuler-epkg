//! # Mirror Performance Metrics and Scoring
//!
//! This module implements the performance scoring algorithm for mirror selection optimization.
//! It calculates weighted performance scores based on historical throughput and latency data,
//! with geographic bonuses for country-localized mirrors.
//!
//! ## Core Metrics
//!
//! - **Throughput**: Historical download speeds in bytes per second (weighted average)
//! - **Latency**: Historical response times in milliseconds (median calculation)
//! - **Performance Score**: Combined metric balancing speed vs. responsiveness
//! - **Geographic Bonus**: 8x multiplier for same-country mirrors
//!
//! ## Scoring Algorithm
//!
//! The performance score is calculated as:
//! ```
//! base_score = avg_throughput / avg_latency
//! final_score = base_score * country_multiplier
//! ```
//!
//! ## Key Features
//!
//! - **Weighted Averages**: Recent measurements have higher influence on scoring
//! - **Bounded Values**: Throughput (1KB/s - 10MB/s) and latency (10ms - 500ms) caps
//! - **Usage Tracking**: Thread-safe counters for active downloads and total usage
//! - **Automatic Cleanup**: History limited to last 10 measurements for efficiency
//! - **Geographic Optimization**: Country-based performance multipliers for localization
//!
//! ## History Management
//!
//! - Maintains rolling history of recent performance measurements
//! - Automatically trims old data to prevent unbounded growth
//! - Supports both throughput and latency tracking per mirror
//! - Thread-safe updates for concurrent access during downloads

use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::mirror::types::*;

impl Mirror {
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
        if let Ok(user_country_code) = crate::location::get_country_code() {
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
