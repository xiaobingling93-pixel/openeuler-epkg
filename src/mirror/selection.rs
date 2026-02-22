//! # Intelligent Mirror Selection System
//!
//! This module implements the core mirror selection algorithm that balances performance
//! optimization with load distribution. It provides sophisticated mirror selection based
//! on historical performance data, geographic proximity, and adaptive connection limiting.
//!
//! ## Selection Strategy
//!
//! ### Performance-Based Ranking
//! - Mirrors are pre-sorted by performance score (throughput/latency ratio)
//! - Geographic bonuses favor local mirrors for better performance and compliance
//! - Historical data from 6 months of logs enables data-driven decisions
//!
//! ### Load Balancing
//! - **Adaptive Connection Limits**: Starts with `pget_limit=1`, grows with parallel task pressure
//! - **Natural Load Distribution**: Fast mirrors complete downloads sooner, becoming available for reuse
//! - **Learned Limits**: Per-mirror connection limits learned from HTTP 429 errors
//! - **Geographic Preferences**: Local mirrors get higher connection limits than remote ones
//!
//! ### Fault Tolerance
//! - **Automatic Fallbacks**: Mirrors marked as unavailable are automatically excluded
//! - **Range Support Checking**: Optional filtering for mirrors supporting HTTP range requests
//! - **Content Integrity**: Exclusion of mirrors with old or inconsistent content
//! - **Metadata Conflicts**: Avoidance of mirrors with known download conflicts
//!
//! ## Key Features
//!
//! - **Thread-Safe Usage Tracking**: Automatic usage counting with RAII-based cleanup
//! - **Performance Analytics**: Comprehensive statistics display for debugging and monitoring
//! - **Configurable Filtering**: Runtime filtering based on download requirements and mirror health
//! - **Efficient Updates**: Available mirror lists updated dynamically as conditions change

use std::sync::atomic::Ordering;
use color_eyre::eyre::{Result, eyre};
use crate::models::channel_config;
use crate::mirror::types::*;
use crate::mirror::loading::initialize_mirrors;

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

/// Protocol type detected from URL or path
#[derive(Debug, Clone, PartialEq)]
pub enum UrlProtocol {
    Http,
    Local,
}

impl Mirrors {

    /// Select mirror with automatic usage tracking
    ///
    /// Returns a Mirror that automatically tracks usage when selected and dropped
    pub fn select_mirror_with_usage_tracking(&mut self, need_range: bool, raw_url: Option<&str>, repodata_name: &str) -> Result<Mirror> {
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
        self.update_available_mirrors(need_range, distro, arch, raw_url, repodata_name);

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
    fn update_available_mirrors(
        &mut self,
        need_range: bool,
        distro: &str,
        arch: &str,
        raw_url: Option<&str>,
        repodata_name: &str,
    ) {
        log::debug!(
            "update_available_mirrors: need_range={}, distro={}, arch={}, repodata_name={}",
            need_range,
            distro,
            arch,
            repodata_name
        );

        let mut filtered_stats = std::collections::HashMap::new();

        self.available_mirrors = self.mirrors.iter()
            .filter_map(|(site, mirror)| {
                // Exclude mirrors with no_content, old_content, or no_online
                if mirror.stats.no_content >= 3 || mirror.stats.old_content || mirror.stats.no_online {
                    log::trace!("Excluding mirror {} due to: no_content={}, old_content={}, no_online={}",
                               site, mirror.stats.no_content, mirror.stats.old_content, mirror.stats.no_online);
                    *filtered_stats.entry("no_content/old_content/no_online").or_insert(0) += 1;
                    return None;
                }

                // If need_range is true, exclude mirrors with no_range=true
                if need_range && mirror.stats.no_range {
                    log::trace!("Excluding mirror {} due to: no_range=true (need_range={})", site, need_range);
                    *filtered_stats.entry("no_range").or_insert(0) += 1;
                    return None;
                }

                // Exclude mirrors that have metadata conflicts with master task
                if let Some(url) = raw_url {
                    if mirror.should_skip_url(url) {
                        log::trace!("Skipping mirror {} for URL {} due to metadata conflicts", mirror.url, url);
                        *filtered_stats.entry("metadata_conflicts").or_insert(0) += 1;
                        return None;
                    }
                }

                // Check if this mirror can provide a valid distro directory
                let distro_dir = Mirrors::find_distro_dir(mirror, distro, arch, repodata_name);
                if distro_dir.is_empty() {
                    log::trace!("Excluding mirror {} due to: no valid distro_dir found for distro={}, arch={}, repodata_name={}",
                               site, distro, arch, repodata_name);
                    *filtered_stats.entry("no_distro_dir").or_insert(0) += 1;
                    return None;
                }

                log::trace!("Mirror {} passed all filters, distro_dir={}", site, distro_dir);
                Some(site.clone())
            })
            .collect();

        if !filtered_stats.is_empty() {
            log::debug!("Mirror filtering summary: {:?}", filtered_stats);
        }
        log::debug!(
            "Available mirrors after filtering: {} out of {} total mirrors",
            self.available_mirrors.len(),
            self.mirrors.len()
        );

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
    if mirror.stats.no_content >= 3 {
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
        println!("=== Available Mirrors ===");

        // Show mirrors not in available_mirrors
        let mut available_count = 0;
        for (site, mirror) in &mirrors.mirrors {
            if !mirrors.available_mirrors.contains(site) {
                available_count += 1;
                show_one_mirror(available_count, site, mirror, mirrors.pget_limit);
            }
        }
    }

    println!("");
    println!("=== End Mirror Stats ===");
}
