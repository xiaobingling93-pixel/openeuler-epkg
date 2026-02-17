//! # Mirror Management Module
//!
//! This module provides comprehensive mirror management functionality for epkg,
//! including intelligent mirror selection, performance tracking, and geographic optimization.
//!
//! ## Core Components
//!
//! - **types**: Core data structures and constants for mirror management
//! - **url**: URL manipulation, path resolution, and mirror URL formatting
//! - **loading**: Mirror initialization with distro and country-based filtering
//! - **logging**: Performance tracking and logging system for mirror analytics
//! - **metrics**: Performance scoring and metrics calculation algorithms
//! - **filtering**: Geographic and exploration-based mirror filtering
//! - **selection**: Intelligent mirror selection with load balancing
//!
//! ## Key Features
//!
//! - Geographic optimization: Country-aware mirror selection for better performance
//! - Performance tracking: 6 months of historical data for intelligent decisions
//! - Load balancing: Adaptive connection limiting and usage distribution
//! - Fault tolerance: Automatic fallback and error recovery mechanisms
//! - Exploration vs exploitation: Gradual discovery of new mirrors while favoring known performers

// Declare submodules
pub mod types;
pub mod url;
pub mod loading;
pub mod logging;
pub mod metrics;
pub mod filtering;
pub mod selection;

// Re-export all types from types module to maintain backward compatibility
pub use types::*;

// Re-export UrlProtocol from selection module for backward compatibility
pub use selection::UrlProtocol;

// Re-export public functions for backward compatibility
pub use logging::{append_download_log, append_http_log};
pub use selection::dump_mirror_performance_stats;
pub use url::url2site;
pub use url::extend_repodata_name2distro_dirs;
