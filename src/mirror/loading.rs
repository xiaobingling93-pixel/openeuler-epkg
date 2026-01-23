//! # Mirror Loading and Initialization
//!
//! This module handles the loading, filtering, and initialization of mirrors from
//! configuration files. It implements the streamlined mirror management system that
//! provides country-aware distro-filtered mirror initialization.
//!
//! ## Key Processes
//!
//! - **Configuration Loading**: Load primary mirrors from `sources/mirrors.json`
//! - **Manual Overrides**: Merge manual mirror configurations from `sources/manual-mirrors.json`
//! - **Distro Filtering**: Filter mirrors to only include those supporting required distributions
//! - **Country Filtering**: Apply geographic optimization based on user location
//! - **Performance Loading**: Bulk load 6 months of historical performance data
//! - **Exploration Filtering**: Balance exploration of new mirrors with exploitation of known performers
//!
//! ## Initialization Flow
//!
//! 1. Load and merge mirror configurations from JSON files
//! 2. Convert URL keys to site keys and merge distro data structures
//! 3. Apply distro-specific filtering based on channel configuration
//! 4. Apply country-based geographic filtering for optimal performance
//! 5. Load comprehensive performance logs for intelligent decision making
//! 6. Calculate initial performance scores for all mirrors
//! 7. Apply exploration filtering to gradually discover new mirrors
//!
//! ## Architecture Benefits
//!
//! - **Single Initialization**: Efficient startup with all filtering applied once
//! - **Geographic Optimization**: Country-based selection when location is available
//! - **Smart Fallbacks**: Ensures adequate mirror availability across different scenarios
//! - **Comprehensive Data**: 6 months of performance history loaded at startup
//! - **Future-Proof**: Extensible key=value logging format for easy feature addition

use std::collections::{HashMap, HashSet};
use color_eyre::eyre::{Result, bail};
use crate::models::channel_config;
use crate::models::channel_configs;
use crate::mirror::types::{Mirror, Mirrors};
use crate::mirror::selection::dump_mirror_performance_stats;
use crate::mirror::url::url2site;
use crate::mirror::logging::load_performance_logs;
use crate::mirror::filtering::{apply_country_code_filtering, filter_mirrors_by_exploration};

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
        if !manual_mirror.probe_dirs.is_empty() {
            existing_mirror.probe_dirs = manual_mirror.probe_dirs.clone();
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

        match crate::io::read_json_file::<HashMap<String, Mirror>>(manual_mirrors_file_path) {
            Ok(manual_mirrors) => {
                log::debug!("Loaded {} manual mirrors", manual_mirrors.len());

                // Merge manual mirrors into all_mirrors_raw
                for (url, manual_mirror) in manual_mirrors {
                    merge_single_manual_mirror(all_mirrors_raw, url, manual_mirror);
                }
            },
            Err(_) => { }
        }
    } else {
        log::debug!("manual-mirrors.json not found, skipping manual mirror loading");
    }

    Ok(())
}


/// Convert URL keys to site keys and merge distros and ls_dirs into distro_dirs
fn convert_mirror_data_structure(all_mirrors_raw: HashMap<String, Mirror>) -> HashMap<String, Mirror> {
    let mut all_mirrors: HashMap<String, Mirror> = HashMap::new();

    for (url, mut mirror) in all_mirrors_raw {
        mirror.distro_dirs.extend(mirror.probe_dirs.clone());
        mirror.distro_dirs.extend(mirror.ls_dirs.clone());
        mirror.distro_dirs.extend(mirror.distros.clone());
        mirror.url = url.clone();

        // Use site name as key instead of full URL
        let site_key = url2site(&url);
        all_mirrors.insert(site_key, mirror);
    }

    all_mirrors
}

/// Apply filtering by channel_config().distro OR channel_config().distro_dirs
fn apply_channel_config_filtering(
    all_mirrors: HashMap<String, Mirror>,
) -> Result<HashMap<String, Mirror>> {
    let distro = &channel_config().distro;

    // Get the union of all distro_dirs from all channel configs
    let mut all_distro_dirs = HashSet::new();
    for config in channel_configs() {
        all_distro_dirs.extend(config.distro_dirs.iter().cloned());
    }
    let distro_dirs: Vec<String> = all_distro_dirs.into_iter().collect();

    let original_count = all_mirrors.len();
    let filtered_mirrors: HashMap<String, Mirror> = all_mirrors
        .into_iter()
        .filter(|(_, mirror)| {
            is_mirror_suitable_for_channel_config(mirror, distro, &distro_dirs)
        })
        .collect();

    log::debug!(
        "Filtered mirrors for distro '{}' OR distro_dirs {:?}: {} out of {} mirrors selected",
        distro,
        distro_dirs,
        filtered_mirrors.len(),
        original_count
    );

    Ok(filtered_mirrors)
}

/// Load sources/mirrors.json with filtering by channel_config().distro OR channel_config().distro_dirs
fn load_mirrors_for_distro() -> Result<HashMap<String, Mirror>> {
    let manager_path = crate::dirs::get_epkg_src_path();
    let mirrors_file_path = manager_path.join("sources/mirrors.json");
    let manual_mirrors_file_path = manager_path.join("sources/manual-mirrors.json");

    // Load primary mirrors.json
    let mut all_mirrors_raw: HashMap<String, Mirror> = crate::io::read_json_file(&mirrors_file_path)?;

    // Load and merge manual-mirrors.json if it exists
    load_and_merge_manual_mirrors(&mut all_mirrors_raw, &manual_mirrors_file_path)?;

    // Convert URL keys to site keys and merge distros and ls_dirs into distro_dirs
    let all_mirrors = convert_mirror_data_structure(all_mirrors_raw);

    // Apply filtering by channel_config().distro AND channel_config().distro_dirs
    let mut filtered_mirrors = apply_channel_config_filtering(all_mirrors)?;

    // Initialize performance scores for all mirrors to ensure they have valid stats
    for mirror in filtered_mirrors.values_mut() {
        // Calculate initial performance score if not already set
        if mirror.stats.score == 0 {
            mirror.calculate_performance_score();
        }
    }

    Ok(filtered_mirrors)
}

/// Check if a mirror is suitable for the channel config, considering distro OR distro_dirs
fn is_mirror_suitable_for_channel_config(mirror: &Mirror, target_distro: &str, target_distro_dirs: &[String]) -> bool {
    // Check if mirror supports the target distro OR any of the required distro_dirs
    let has_distro = mirror.distros.contains(&target_distro.to_string());
    let has_dirs = target_distro_dirs.iter().any(|required_dir| {
        mirror.distro_dirs.contains(required_dir)
    });

    // Mirror must support either the distro OR the distro_dirs
    if !has_distro && !has_dirs {
        return false;
    }

    let arch = &channel_config().arch;

    // Fedora-specific architecture rules
    if target_distro == "fedora" {
        if arch != "x86_64" && arch != "aarch64" {
            // For non-primary architectures, mirror must support secondary repos
            return mirror.distro_dirs.iter().any(|dir| dir.contains("secondary"));
        }
    }

    // Ubuntu-specific architecture rules
    if target_distro == "ubuntu" {
        if arch != "x86_64" {
            // For non-x86_64 architectures, mirror must support ports
            return mirror.distro_dirs.iter().any(|dir| dir.contains("ports"));
        }
    }

    true
}

/// Initialize mirrors with distro and country code filtering
pub(crate) fn initialize_mirrors() -> Result<Mirrors> {
    let mirrors = match load_mirrors_for_distro() {
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
