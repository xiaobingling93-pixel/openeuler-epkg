// ============================================================================
// DOWNLOAD MODULE - Module Declaration and Public API
//
// This module provides the core download functionality for epkg, implementing
// parallel, resumable downloads with mirror support, chunking, and integrity
// validation. It consists of multiple submodules that handle different aspects
// of the download process.
//
// Key Components:
// - types: Core data structures and type definitions
// - manager: Download manager with concurrency control
// - orchestration: High-level download coordination
// - chunk: Parallel chunked download system
// - http: HTTP client operations and response handling
// - aur: Arch User Repository (AUR) specific handling
// - mirror: Mirror selection and synchronization
// - validation: Download integrity and range validation
// - progress: Progress bar and status tracking
// - task: Individual download task management
// - utils: Shared utility functions
// - file_ops: File system operations and metadata handling
// ============================================================================

// Declare submodules
pub mod types;
pub mod utils;
pub mod aur;
pub mod validation;
pub mod progress;
pub mod mirror;
pub mod file_ops;
pub mod manager;
pub mod task;
pub mod http;
pub mod chunk;
pub mod orchestration;

// Re-export all types from types module to maintain backward compatibility
pub use types::*;

// Re-export orchestration functions for public API
pub use orchestration::{download_urls, enqueue_package_downloads, get_package_file_path};

// Re-export manager functions and statics for public API
pub use manager::{DOWNLOAD_MANAGER, submit_download_task, has_download_task, wait_for_any_download_task, cancel_downloads};

// Internal functions accessible within the crate
pub(crate) use progress::setup_task_progress_tracking;
pub(crate) use mirror::extract_server_metadata;
pub(crate) use file_ops::should_redownload;
