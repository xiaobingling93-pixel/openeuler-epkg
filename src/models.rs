use std::collections::{HashMap, HashSet, BTreeMap};
use std::os::unix::net::UnixStream;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::LazyLock;
use crate::parse_options_common;
use crate::parse_options_subcommand;
use std::sync::Arc;
use crate::search::SearchOptions;
use color_eyre::Result;
use color_eyre::eyre;


pub const SUPPORT_ARCH_LIST: &[&str] = &["aarch64", "x86_64", "riscv64", "loongarch64"];
pub const PURE_ENV_SUFFIX: char = '!';
pub const DEFAULT_CHANNEL: &str = &"debian";
pub const DEFAULT_COMMIT:  &str = &env!("EPKG_VERSION_TAG"); // epkg self install will download this commit from gitee

pub const SELF_ENV: &str = &"self"; // holds epkg, elf-loader, package-manager source files
pub const MAIN_ENV: &str = &"main"; // the default env for most operations, must be private

// Package format types
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub enum PackageFormat {
    #[serde(rename = "epkg")]
    Epkg,
    #[serde(rename = "deb")]
    Deb,
    #[serde(rename = "rpm")]
    Rpm,
    #[serde(rename = "apk")]
    Apk,
    #[serde(rename = "pacman")]
    Pacman,
    #[serde(rename = "conda")]
    Conda,
    #[serde(rename = "python")]
    Python,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    pub pkgname: String,
    pub ca_hash: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub arch: String,
}

// Structure to hold begin offset and length for a package
#[derive(Debug, Clone)]
pub struct PackageRange {
    pub begin: usize,
    pub len: usize,
}

// $HOME/.cache/epkg/channels/debian:trixie/main/x86_64/packages-all.txt
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Package {
    pub pkgname: String,
    pub version: String,
    #[serde(default = "default_arch")]
    pub arch: String,

    #[serde(default)]
    pub size: u32,
    #[serde(default)]
    #[serde(rename = "installedSize")]
    pub installed_size: u32,
    #[serde(default)]
    #[serde(rename = "buildTime")]
    pub build_time: Option<u32>,

    #[serde(default)]
    #[serde(rename = "source")]
    pub source: Option<String>,
    #[serde(default)]
    pub location: String,

    // caHash is only available in installed epkg_store/fs/package.txt,
    // when the struct is loaded by map_pkgline2package()
    #[serde(default)]
    #[serde(rename = "caHash")]
    pub ca_hash: Option<String>,

    // Apk only has sha1sum; other formats only have sha256sum
    #[serde(default)]
    #[serde(rename = "sha256")]
    pub sha256sum: Option<String>,
    #[serde(default)]
    #[serde(rename = "sha1")]
    pub sha1sum: Option<String>,

    #[serde(skip)]
    #[allow(dead_code)]
    pub depends: Vec<Dependency>,
    #[serde(default)]
    #[serde(rename = "requiresPre")]
    pub requires_pre: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    #[serde(rename = "buildRequires")]
    pub build_requires: Vec<String>,
    #[serde(default)]
    #[serde(rename = "checkRequires")]
    pub check_requires: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub recommends: Vec<String>,
    #[serde(default)]
    pub suggests: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub obsoletes: Vec<String>,
    #[serde(default)]
    pub enhances: Vec<String>,
    #[serde(default)]
    pub supplements: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,

    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub maintainer: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    #[serde(rename = "originUrl")]
    pub origin_url: Option<String>,
    #[serde(default)]
    #[serde(rename = "multiArch")]
    pub multi_arch: Option<String>,

    #[serde(default)]
    pub format: PackageFormat,

    #[serde(default)] // necessary for solver_tests::tests
    #[serde(rename = "repo")]
    pub repodata_name: String,

    #[serde(default)] // necessary for solver_tests::tests
    pub pkgkey: String, // != pkgline

    #[serde(skip)]
    #[serde(rename = "packageBaseurl")]
    pub package_baseurl: String,
}

// pkgline format: {ca_hash}__{pkgname}__{version}__{arch}
// 09c88c8eb9820a3570d9a856b91f419c__libselinux__3.3-5.oe2203sp3__x86_64
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct StorePathsIndex {
    pub filename: String,
    #[serde(default)]
    pub sha256sum: Option<String>,
    #[serde(default)]
    pub datetime: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct FilelistsFileInfo {
    pub filename: String,
    pub sha256sum: String,
    pub datetime: String,
    pub size: u64,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PackagesFileInfo {
    pub filename: String,
    pub sha256sum: String,
    pub datetime: String,
    pub size: u64,
    pub nr_packages: usize,
    pub nr_provides: usize,
    pub nr_essentials: usize,
}

/*
    # ${HOME}/.epkg/envs/main/generations/current/installed-packages.json
    {
      "${pkgkey}": {
      },
	  "jq__1.8.0-1__x86_64": {
		"pkgline": "g5zo2bniyoyf3jwx4vo25qrf46al7ric__jq__1.8.0-1__x86_64",
		"arch": "x86_64",
		"depend_depth": 0,
		"install_time": 1749433093,
		"ebin_exposure": true,
		"rdepends": []
	  },
	  "filesystem__2025.05.03-1__any": {
		"pkgline": "7noavnmhiezcdzrjiv3tyhupsi2w4etw__filesystem__2025.05.03-1__any",
		"arch": "any",
		"depend_depth": 2,
		"install_time": 1749437271,
		"ebin_exposure": false,
		"rdepends": [
		  "glibc__2.41+r48+g5cb575ca9a3d-1__x86_64"
		]
	  },
    }
*/
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstalledPackageInfo {
    // pkgline format is: {ca_hash}__{pkgname}__{version}__{arch}
    // that means pkgline={ca_hash}__{pkgkey}
    #[serde(default)]  // empty for solver test data
    pub pkgline: String,
    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default)]
    pub depend_depth: u16,
    #[serde(default)]
    pub install_time: u64,

    // ebin_exposure=true if:
    // (1) package is user-requested (depend_depth == 0), OR
    // (2) package is a dependency whose 'source' package matches the 'source' of any user-requested package.
    // Otherwise, false. Set by `record_appbin_source`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ebin_exposure: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rdepends: Vec<String>, // Stores pkgkeys of packages that depend on this one
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<String>, // Stores pkgkeys of packages this package depends on
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bdepends: Vec<String>, // Stores pkgkeys of build dependencies (Pacman only)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rbdepends: Vec<String>, // Stores pkgkeys of packages that have this as a build dependency (Pacman only)
    #[serde(default, skip_serializing_if = "Vec::is_empty")] // for backward compatibility with older installed-packages.json
    pub ebin_links: Vec<String>,
}

impl Default for InstalledPackageInfo {
    fn default() -> Self {
        InstalledPackageInfo {
            pkgline: String::new(),
            arch: default_arch(),
            depend_depth: 0,
            install_time: 0,
            ebin_exposure: false,
            rdepends: Vec::new(),
            depends: Vec::new(),
            bdepends: Vec::new(),
            rbdepends: Vec::new(),
            ebin_links: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct GenerationCommand {
    pub timestamp: String,
    pub action: String,
    pub command_line: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fresh_installs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upgrades_new: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upgrades_old: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub old_removes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_exposes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub del_exposes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct EnvConfig {
    pub name: String,
    pub env_base: String,
    pub env_root: String,

    #[serde(default, skip_serializing_if = "is_false")]
    pub public: bool,

    #[serde(default, skip_serializing_if = "is_false")]
    pub register_to_path: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub register_priority: i32,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env_vars: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct EnvExport {
    #[serde(flatten)]
    pub env: EnvConfig,

    #[serde(default)]
    pub packages: HashMap<String, InstalledPackageInfo>,        // key is pkgkey (!= pkgline)

    #[serde(default)]
    pub world: HashMap<String, String>,
}

// # ChannelConfig is loaded from ${env_root}/etc/epkg/channel.yaml
// # On `epkg self install`, may copy from $EPKG_SRC/sources/${channel}.yaml
// distro: "openeuler"
// version: "24.03-lts"
// index_url: "https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-$VERSION/$repo/$arch/repodata/repomd.xml"
//
// repos:
//   everything:
//     # index_url: defaults to top level index_url
//     # enabled: false # defaults to true
//   mysql:
//     # a repo can specify its own url
//     index_url: "http://third.party/repo/dir/"

// Implement Default for PackageFormat
impl Default for PackageFormat {
    fn default() -> Self {
        PackageFormat::Epkg
    }
}

impl PackageFormat {
    /// Convert PackageFormat to its string representation
    pub fn to_str(self) -> &'static str {
        match self {
            PackageFormat::Epkg => "epkg",
            PackageFormat::Deb => "deb",
            PackageFormat::Rpm => "rpm",
            PackageFormat::Apk => "apk",
            PackageFormat::Pacman => "pacman",
            PackageFormat::Conda => "conda",
            PackageFormat::Python => "python",
        }
    }

    /// Parse a string into PackageFormat, returning an error for unknown formats
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "epkg"      => Ok(PackageFormat::Epkg),
            "deb"       => Ok(PackageFormat::Deb),
            "rpm"       => Ok(PackageFormat::Rpm),
            "apk"       => Ok(PackageFormat::Apk),
            "pacman"    => Ok(PackageFormat::Pacman),
            "conda"     => Ok(PackageFormat::Conda),
            "python"    => Ok(PackageFormat::Python),
            _ => Err(eyre::eyre!("Unknown format: {}", s)),
        }
    }

    /// Convert PackageFormat to its file suffix/extension
    #[allow(unreachable_patterns)]
    pub fn to_suffix(self) -> Result<&'static str> {
        match self {
            PackageFormat::Rpm => Ok("rpm"),
            PackageFormat::Deb => Ok("deb"),
            PackageFormat::Apk => Ok("apk"),
            PackageFormat::Pacman => Ok("pkg.tar.zst"),
            PackageFormat::Conda => Ok("conda"),
            PackageFormat::Epkg => Ok("epkg"),
            PackageFormat::Python => Ok("whl"),
            _ => unreachable!("All PackageFormat variants are covered"),
        }
    }

    /// Parse a file suffix/extension into PackageFormat
    /// Handles both full filenames (e.g., "package.deb", "package.pkg.tar.xz") and extensions (e.g., "deb", "pkg.tar.xz")
    pub fn from_suffix(suffix: &str) -> Result<Self> {
        // Normalize: remove leading dot if present
        let suffix = suffix.strip_prefix('.').unwrap_or(suffix);

        // Check multi-part suffixes first (longer matches first)
        if suffix.ends_with("pkg.tar.zst") || suffix.ends_with("pkg.tar.xz") {
            return Ok(PackageFormat::Pacman);
        }
        if suffix.ends_with("tar.bz2") {
            return Ok(PackageFormat::Conda);
        }
        if suffix.ends_with("tar.gz") {
            return Ok(PackageFormat::Python);
        }

        // For single-part extensions, check the last part after the last dot
        // This handles both "package.deb" -> "deb" and just "deb" -> "deb"
        let ext = if let Some(dot_pos) = suffix.rfind('.') {
            &suffix[dot_pos + 1..]
        } else {
            suffix
        };

        match ext {
            "deb"   => Ok(PackageFormat::Deb),
            "rpm"   => Ok(PackageFormat::Rpm),
            "epkg"  => Ok(PackageFormat::Epkg),
            "apk"   => Ok(PackageFormat::Apk),
            "conda" => Ok(PackageFormat::Conda),
            "whl"   => Ok(PackageFormat::Python),
            _ => Err(eyre::eyre!("Unknown package format suffix: {}", suffix)),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct ChannelConfig {
    pub format: PackageFormat,
    pub distro: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub distro_dirs: Vec<String>,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub arch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    #[serde(default)]
    pub repos: HashMap<String, RepoConfig>, // point to online repo, key: repo_name

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<String>,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_versions: Vec<String>,

    pub index_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_url_noarch: Option<String>, // conda https://mirrors.tuna.tsinghua.edu.cn/anaconda/pkgs/r/noarch/
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_url_updates: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_url_security: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_url_nonfree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_url_nonfree_updates: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>, // filename for repos.d configs
    #[serde(skip_serializing, skip_deserializing)]
    #[serde(default)]
    pub file_data: String, // original file data to preserve during save
}

pub fn default_as_true() -> bool { true }

#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct RepoConfig {
    #[serde(default = "default_as_true")]
    pub enabled: bool,
    #[serde(default)]
    pub index_url: Option<String>,
    #[serde(default)]
    pub index_url_noarch: Option<String>,
    #[serde(default)]
    pub index_url_updates: Option<String>,
    #[serde(default)]
    pub index_url_security: Option<String>,
    #[serde(default)]
    pub index_url_nonfree: Option<String>,
    #[serde(default)]
    pub index_url_nonfree_updates: Option<String>,
    #[serde(default)]
    pub package_baseurl: String, // auto computed from url and ChannelInfo.baseurl
}

static REPODATA_INDICE: LazyLock<std::sync::RwLock<HashMap<String, RepoIndex>>> =
        LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

// Global ENV_CONFIG and CHANNEL_CONFIGS using LazyLock
static ENV_CONFIG: LazyLock<EnvConfig> = LazyLock::new(|| {
    // During tests, config() might not be available, so provide a default
    #[cfg(test)]
    {
        // Return a minimal default config for tests
        return EnvConfig::default();
    }
    #[cfg(not(test))]
    {
        crate::io::deserialize_env_config().expect("Failed to deserialize env config")
    }
});

static CHANNEL_CONFIGS: LazyLock<Vec<ChannelConfig>> = LazyLock::new(|| {
    // During tests, config() might not be available, so provide a default
    #[cfg(test)]
    {
        // Return empty vec for tests
        return Vec::new();
    }
    #[cfg(not(test))]
    {
        crate::io::deserialize_channel_config().expect("Failed to deserialize channel config")
    }
});

// Accessor functions for global configs
pub fn env_config() -> &'static EnvConfig {
    &ENV_CONFIG
}

pub fn channel_configs() -> &'static Vec<ChannelConfig> {
    &CHANNEL_CONFIGS
}

#[cfg(test)]
static DEFAULT_CHANNEL_CONFIG: LazyLock<Mutex<ChannelConfig>> = LazyLock::new(|| Mutex::new(ChannelConfig::default()));

pub fn channel_config() -> &'static ChannelConfig {
    // During tests, CHANNEL_CONFIGS might be empty
    #[cfg(test)]
    {
        static CONFIG_PTR: OnceLock<usize> = OnceLock::new();
        if CHANNEL_CONFIGS.is_empty() {
            let mutex = LazyLock::force(&DEFAULT_CHANNEL_CONFIG);
            let config = mutex.lock().unwrap();

            // SAFETY: Similar to config() implementation - we store a raw pointer
            // to data inside a static Mutex and return it as a static reference.
            // This is safe because:
            // 1. DEFAULT_CHANNEL_CONFIG is a static LazyLock, so it lives for the entire program
            // 2. The Mutex ensures thread safety
            // 3. We're only reading, and the pointer points to data in the static Mutex
            let ptr_usize = *CONFIG_PTR.get_or_init(|| {
                &*config as *const ChannelConfig as usize
            });
            return unsafe { &*(ptr_usize as *const ChannelConfig) };
        }
    }
    &CHANNEL_CONFIGS[0]
}

#[cfg(test)]
/// Get mutable access to the channel config for test customization.
/// This function locks the Mutex, so it should be used carefully in tests.
pub fn channel_config_mut() -> std::sync::MutexGuard<'static, ChannelConfig> {
    if CHANNEL_CONFIGS.is_empty() {
        // In test mode, we use Mutex for interior mutability
        DEFAULT_CHANNEL_CONFIG.lock().unwrap()
    } else {
        // This shouldn't happen in tests, but handle it gracefully
        panic!("channel_config_mut() called but CHANNEL_CONFIGS is not empty");
    }
}

// use at package install time
pub fn repodata_indice() -> std::sync::RwLockReadGuard<'static, HashMap<String, RepoIndex>> {
    REPODATA_INDICE.read().expect("Failed to read repodata index")
}

// use at repo update time
pub fn repodata_indice_mut() -> std::sync::RwLockWriteGuard<'static, HashMap<String, RepoIndex>> {
    REPODATA_INDICE.write().expect("Failed to write repodata index")
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RepoIndex {
    pub repodata_name: String,
    #[serde(skip)]
    pub package_baseurl: String,
    #[serde(skip)]
    pub repo_dir_path: String,
    #[serde(default)]
    pub format: PackageFormat,
    pub repo_shards: HashMap<String, RepoShard>, // key: shard name or id
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RepoShard {
    #[serde(default)]
    pub packages: PackagesFileInfo,
    #[serde(default)]
    pub filelists: Option<FilelistsFileInfo>,

    #[serde(skip)]
    pub provide2pkgnames: HashMap<String, Vec<String>>,
    #[serde(skip)]
    pub essential_pkgnames: HashSet<String>,
    #[serde(skip)]
    pub pkgname2ranges: BTreeMap<String, Vec<PackageRange>>,
    #[serde(skip)]
    pub packages_mmap: Option<crate::mmio::FileMapper>,
    #[serde(skip)]
    pub pkgname2ranges_path: Option<std::path::PathBuf>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum EpkgCommand {
    #[default]
    None,
    Env,
    List,
    Info,
    Install,
    Upgrade,
    Remove,
    History,
    Restore,
    Update,
    Repo,
    Hash,
    Build,
    Unpack,
    Convert,
    Run,
    Search,
    Gc,
    SelfInstall,
    SelfUpgrade,
    SelfRemove,
}

impl From<&str> for EpkgCommand {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "env" => EpkgCommand::Env,
            "list" => EpkgCommand::List,
            "info" => EpkgCommand::Info,
            "install" => EpkgCommand::Install,
            "upgrade" => EpkgCommand::Upgrade,
            "remove" => EpkgCommand::Remove,
            "history" => EpkgCommand::History,
            "restore" => EpkgCommand::Restore,
            "update" => EpkgCommand::Update,
            "repo" => EpkgCommand::Repo,
            "hash" => EpkgCommand::Hash,
            "build" => EpkgCommand::Build,
            "unpack" => EpkgCommand::Unpack,
            "convert" => EpkgCommand::Convert,
            "run" => EpkgCommand::Run,
            "search" => EpkgCommand::Search,
            "gc" => EpkgCommand::Gc,
            "self" => EpkgCommand::None, // Handled separately for nested subcommands
            _ => EpkgCommand::None, // Default for empty or unrecognized strings
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EPKGConfig {
    #[serde(default = "default_common_options")]
    pub common: CommonOptions,
    #[serde(default)]
    pub install: InstallOptions,
    #[serde(default)]
    pub upgrade: UpgradeOptions,
    #[serde(default)]
    pub update: UpdateOptions,
    #[serde(default)]
    pub list: ListOptions,
    #[serde(default)]
    pub env: EnvOptions,
    #[serde(default)]
    pub history: HistoryOptions,
    #[serde(default = "default_init_options")]
    pub init: InitOptions,
    #[serde(skip)]
    pub search: SearchOptions,

    #[serde(skip)]
    pub config_file: String,
    #[serde(skip)]
    pub command_line: String,
    #[serde(skip)]
    pub subcommand: EpkgCommand,
}

// Custom default function that ensures serde field-level defaults are applied
fn default_common_options() -> CommonOptions {
    // Use serde to deserialize an empty object, which will trigger field-level defaults
    serde_yaml::from_str("{}").unwrap_or_else(|_| CommonOptions::default())
}

// Custom default function that ensures serde field-level defaults are applied
fn default_init_options() -> InitOptions {
    // Use serde to deserialize an empty object, which will trigger field-level defaults
    serde_yaml::from_str("{}").unwrap_or_else(|_| InitOptions::default())
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct CommonOptions {
    #[serde(skip)]
    pub env: String,
    #[serde(skip)]
    pub download_only: bool,
    #[serde(skip)]
    pub dry_run: bool,

    #[serde(default = "default_arch")]
    pub arch: String,
    #[serde(default)]
    pub quiet: bool,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub assume_yes: bool,
    #[serde(default)]
    pub assume_no: bool,
    #[serde(default)]
    pub ignore_missing: bool,

    // N: expire after N seconds
    // 0: never expire (the default)
    // -1: always expire
    #[serde(default)]
    pub metadata_expire: i32,

    #[serde(default)]
    pub proxy: String,

    // Default: 6 retries for download tasks
    #[serde(default = "default_nr_retry")]
    pub nr_retry: usize,

    // Default: 6 parallel download threads
    // If user sets <= 0, it gets adjusted to at least 1 in the implementation
    #[serde(default = "default_nr_parallel_download")]
    pub nr_parallel_download: usize,

    // Default: auto-enabled if nr_cpu >= 4 && memory >= 1G, else auto-disabled
    // If user specifies nr_parallel <= 1, this gets auto-disabled
    // Parallel processing speeds up `epkg update` at cost of more memory
    #[serde(default = "default_parallel_processing")]
    pub parallel_processing: bool,
}

// Default function for arch
pub fn default_arch() -> String {
    std::env::consts::ARCH.to_string()
}

fn default_nr_retry() -> usize {
    6
}

fn default_nr_parallel_download() -> usize {
    6
}

// Default function for parallel_processing
// Auto-enabled if nr_cpu >= 4 && memory >= 1G, else auto-disabled
fn default_parallel_processing() -> bool {
    // Default nr_parallel is 6, so we don't need to check it here
    // as it will be checked at runtime in setup_parallel_params

    // Check CPU count
    let num_cpus = num_cpus::get();
    let has_enough_cpus = num_cpus >= 4;

    // Check memory
    let has_enough_memory = match sys_info::mem_info() {
        Ok(mem) => {
            // mem.total is in KB, so 1GB = 1024 * 1024 KB
            mem.total >= 1024 * 1024
        },
        Err(_) => false, // If we can't determine memory, assume not enough
    };

    has_enough_cpus && has_enough_memory
}

// Helper function for skip_serializing_if to skip false boolean values
fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct InstallOptions {
    #[serde(default)]
    pub install_suggests: bool,
    #[serde(default)]
    pub no_install_recommends: bool,
    #[serde(default)]
    pub no_install_essentials: bool,
    #[serde(skip)]
    pub no_install: String, // Original cmdline string for --no-install (e.g., "pkg1,pkg2,-pkg3")
    #[serde(default)]
    pub prefer_low_version: bool,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct UpgradeOptions {
    /// Full upgrade mode: upgrade all packages, not just those in world.json
    /// When true and command is Upgrade, get_candidates() won't favor any packages
    #[serde(skip)]
    pub full_upgrade: bool,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct UpdateOptions {
    /// Whether to download filelists (needed for file/path search)
    #[serde(skip)]
    pub need_files: bool,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct ListOptions {
    #[serde(default)]
    pub list_all: bool,
    #[serde(default)]
    pub list_installed: bool,
    #[serde(default)]
    pub list_available: bool,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct EnvOptions {
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub pure: bool,
    #[serde(default)]
    pub stack: bool,
    #[serde(default)]
    pub repos: Vec<String>,

    #[serde(skip)]
    pub env_path: Option<String>,
    #[serde(skip)]
    pub import_file: Option<String>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct HistoryOptions {
    #[serde(default)]
    pub max_generations: Option<u32>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct InitOptions {
    #[serde(skip)]
    pub shared_store: bool,
    #[serde(default = "default_commit")]
    pub commit: String,
}

pub fn default_commit() -> String {
    DEFAULT_COMMIT.to_string()
}

#[derive(Debug)]
pub struct EPKGDirs {
    // Base directories
    pub opt_epkg: PathBuf,
    pub home_epkg: PathBuf,

    // Subdirectories

    // Per-User dirs
    // - If  shared_store:  /opt/epkg/envs/$USER
    // - If !shared_store:  $HOME/.epkg/envs
    pub user_envs: PathBuf,
    // - If  shared_store:  /opt/epkg/cache/aur_builds/$USER
    // - If !shared_store:  $HOME/.cache/epkg/aur_builds
    pub user_aur_builds: PathBuf,

    pub epkg_store: PathBuf,
    pub epkg_cache: PathBuf,
    pub epkg_downloads_cache: PathBuf,
    pub epkg_channels_cache: PathBuf,
}

#[derive(Default)]
pub struct PackageManager {
    // cache need to installing packages info
    pub pkgkey2package: HashMap<String, Arc<Package>>,
    pub pkgline2package: HashMap<String, Arc<Package>>, // cache for locally installed packages

    // Performance indexes for fast lookups (maintained when packages are added to pkgkey2package)
    // Index: pkgname -> Vec<Arc<Package>> for O(1) lookup by package name
    pub pkgname2packages: HashMap<String, Vec<Arc<Package>>>,
    // Index: provide_name -> HashSet<pkgname> for O(1) provider lookup
    pub provide2pkgnames: HashMap<String, HashSet<String>>,

    // loaded from env installed-packages.json
    // `self.installed_packages` (loaded from installed-packages.json) is the
    // authoritative data source. If a pkgkey is not found here, the package
    // is treated as not installed.
    pub installed_packages: HashMap<String, InstalledPackageInfo>, // key is pkgkey (!= pkgline)

    // loaded from env world.json
    // `self.world` maintains top-level package constraints (name -> version_constraint)
    // where version_constraint is normally empty string, or e.g., "=version1", ">=version2"
    // Special key "no-install" stores space-separated list of package names to exclude
    pub world: HashMap<String, String>, // key is pkgname, value is version constraint string

    pub has_worker_process: bool,
    pub ipc_socket: String,
    pub ipc_stream: Option<UnixStream>,
    pub child_pid: Option<nix::unistd::Pid>,
}

pub static CLAP_MATCHES: LazyLock<clap::ArgMatches> = LazyLock::new(|| {
    // During tests, create a minimal ArgMatches to avoid command-line parsing errors
    // This allows tests to run without requiring valid epkg command-line arguments
    #[cfg(test)]
    {
        use clap::{Command, Arg, ArgAction};
        Command::new("epkg")
            .arg(Arg::new("arch").long("arch").default_value(std::env::consts::ARCH))
            .arg(Arg::new("env").short('e').long("env"))
            .arg(Arg::new("config").short('C').long("config"))
            .arg(Arg::new("dry-run").long("dry-run").action(ArgAction::SetTrue))
            .arg(Arg::new("download-only").long("download-only").action(ArgAction::SetTrue))
            .arg(Arg::new("quiet").short('q').long("quiet").action(ArgAction::SetTrue))
            .arg(Arg::new("verbose").short('v').long("verbose").action(ArgAction::SetTrue))
            .arg(Arg::new("assume-yes").short('y').long("assume-yes").action(ArgAction::SetTrue))
            .arg(Arg::new("assume-no").long("assume-no").action(ArgAction::SetTrue))
            .arg(Arg::new("ignore-missing").short('m').long("ignore-missing").action(ArgAction::SetTrue))
            .arg(Arg::new("metadata-expire").long("metadata-expire"))
            .arg(Arg::new("proxy").long("proxy"))
            // Keep test ArgMatches in sync with options used in parse_options_common/setup_parallel_params
            .arg(
                Arg::new("retry")
                    .long("retry")
                    .value_parser(clap::value_parser!(usize)),
            )
            .arg(
                Arg::new("parallel-download")
                    .long("parallel-download")
                    .value_parser(clap::value_parser!(usize)),
            )
            .arg(
                Arg::new("parallel-processing")
                    .long("parallel-processing")
                    .value_parser(clap::value_parser!(bool)),
            )
            // Add a dummy subcommand so parsing doesn't fail
            .subcommand(Command::new("info").arg(Arg::new("PACKAGE_SPEC").num_args(0..)))
            .arg_required_else_help(false) // Don't require subcommand during tests
            .get_matches_from(vec!["epkg", "--dry-run", "info"]) // Use a valid subcommand with dry-run enabled
    }
    #[cfg(not(test))]
    {
        use crate::parse_cmdline;
        parse_cmdline()
    }
});

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
static CONFIG_MUTEX: LazyLock<Mutex<EPKGConfig>> = LazyLock::new(|| {
    let matches = &CLAP_MATCHES;
    let config = parse_options_common(&matches)
        .expect("Failed to parse common options for CONFIG");
    let config = parse_options_subcommand(&matches, config)
        .expect("Failed to parse subcommand options for CONFIG");
    Mutex::new(config)
});

#[cfg(test)]
static CONFIG_PTR: OnceLock<usize> = OnceLock::new();

#[cfg(not(test))]
static CONFIG: LazyLock<EPKGConfig> = LazyLock::new(|| {
    let matches = &CLAP_MATCHES;
    let config = parse_options_common(&matches)
        .expect("Failed to parse common options for CONFIG");
    parse_options_subcommand(&matches, config)
        .expect("Failed to parse subcommand options for CONFIG")
});

static DIRS: LazyLock<EPKGDirs> = LazyLock::new(|| {
    EPKGDirs::build_dirs(&config())
        .expect("Failed to initialize EPKGDirs")
});

#[cfg(test)]
pub fn config() -> &'static EPKGConfig {
    // SAFETY: In test mode, we use a Mutex for interior mutability.
    // We initialize a raw pointer (as usize) once and reuse it. This is safe because:
    // 1. CONFIG_MUTEX is a static LazyLock, so it lives for the entire program
    // 2. The Mutex ensures thread safety for mutations
    // 3. We're only reading, and the pointer points to data in the static Mutex
    unsafe {
        let ptr_usize = *CONFIG_PTR.get_or_init(|| {
            let mutex = LazyLock::force(&CONFIG_MUTEX);
            let guard = mutex.lock().unwrap();
            &*guard as *const EPKGConfig as usize
        });
        &*(ptr_usize as *const EPKGConfig)
    }
}

#[cfg(not(test))]
pub fn config() -> &'static EPKGConfig {
    &CONFIG
}

#[cfg(test)]
/// Get mutable access to the global config for test customization.
/// This function locks the Mutex, so it should be used carefully in tests.
pub fn config_mut() -> std::sync::MutexGuard<'static, EPKGConfig> {
    // In test mode, we use Mutex for interior mutability
    CONFIG_MUTEX.lock().unwrap()
}

pub fn dirs() -> &'static EPKGDirs {
    &DIRS
}
