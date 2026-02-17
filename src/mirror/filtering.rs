//! # Mirror Filtering and Selection Logic
//!
//! This module implements intelligent filtering strategies for mirror selection,
//! balancing geographic proximity, performance data availability, and exploration
//! of new mirrors to optimize download performance and reliability.
//!
//! ## Filtering Strategies
//!
//! ### Country-Based Filtering
//! - **Local Mirrors**: Mirrors in the same country as the user (highest priority)
//! - **EU Mirrors**: For EU countries, includes all European mirrors for better coverage
//! - **World Mirrors**: Global mirrors with performance-based selection limits
//! - **Smart Fallbacks**: Ensures adequate mirror availability when local options are limited
//!
//! ### Exploration Filtering
//! - **Performance-Based**: Prioritizes mirrors with historical throughput data
//! - **Adaptive Exploration**: Gradually increases exploration as more mirrors gain performance data
//! - **Balanced Approach**: Shifts from exploration to exploitation as data accumulates
//!
//! ## Geographic Logic
//!
//! - **EU Special Handling**: EU countries get access to all European mirrors for comprehensive coverage
//! - **Progressive Inclusion**: Remote mirrors are included only when local mirror count is insufficient
//! - **Performance Thresholds**: World mirrors are selected by performance score ranking
//!
//! ## Exploration Dynamics
//!
//! The exploration filter implements a gradual shift from discovery to optimization:
//! - When few mirrors have data: Aggressive exploration of new mirrors
//! - When many mirrors have data: Conservative exploration, favoring known performers
//! - Dynamic thresholds based on the ratio of mirrors with vs. without performance history

use std::collections::HashMap;
use crate::location;
use crate::mirror::types::{Mirror, ENOUGH_LOCAL_MIRRORS, INCLUDE_WORLD_MIRRORS, RATIO_MIRRORS_FOR_EXPLORATION};

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
pub(crate) fn apply_country_code_filtering(mirrors: HashMap<String, Mirror>) -> HashMap<String, Mirror> {
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
pub(crate) fn filter_mirrors_by_exploration(mirrors: &mut HashMap<String, Mirror>) {
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

