use std::collections::{HashMap, HashSet, BTreeMap};
use std::os::unix::net::UnixStream;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::LazyLock;
use crate::parse_cmdline;
use crate::parse_options_common;
use crate::parse_options_subcommand;
use std::sync::Arc;
use crate::search::SearchOptions;


pub const SUPPORT_ARCH_LIST: &[&str] = &["aarch64", "x86_64", "riscv64", "loongarch64"];
pub const PURE_ENV_SUFFIX: char = '!';
pub const DEFAULT_CHANNEL: &str = &"debian";
pub const DEFAULT_VERSION: &str = &"master"; // epkg init will download this version from gitee

pub const BASE_ENV: &str = &"base"; // holds epkg, elf-loader, package-manager source files
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

// $HOME/.cache/epkg/channel/debian:trixie/main/x86_64/packages-all.txt
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Package {
    pub pkgname: String,
    pub version: String,
    #[serde(default)]
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
    pub depends: Vec<Dependency>,
    #[serde(default)]
    #[serde(rename = "requiresPre")]
    pub requires_pre: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub recommends: Vec<String>,
    #[serde(default)]
    pub suggests: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,

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

    #[serde(skip)]
    pub pkgkey: String, // != pkgline
    #[serde(skip)]
    #[serde(rename = "repodataName")]
    pub repodata_name: String,
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
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstalledPackageInfo {
    // pkgline format is: {ca_hash}__{pkgname}__{version}__{arch}
    // that means pkgline={ca_hash}__{pkgkey}
    pub pkgline: String,
    pub arch: String,
    pub depend_depth: u16,
    #[serde(default)]
    pub install_time: u64,

    // ebin_exposure=true if:
    // (1) package is user-requested (depend_depth == 0), OR
    // (2) package is a dependency whose 'source' package matches the 'source' of any user-requested package.
    // Otherwise, false. Set by `record_appbin_source`.
    #[serde(default)]
    pub ebin_exposure: bool,

    #[serde(default)] // Default to empty Vec if missing during deserialization
    pub rdepends: Vec<String>, // Stores pkgkeys of packages that depend on this one
    #[serde(default)] // Default to empty Vec if missing during deserialization
    pub depends: Vec<String>, // Stores pkgkeys of packages this package depends on
    #[serde(default)] // for backward compatibility with older installed-packages.json
    pub ebin_links: Vec<String>,
}

impl InstalledPackageInfo {
    #[allow(dead_code)]
    pub fn new(pkgline: String, arch: String, depend_depth: u16, ebin_exposure: bool) -> Self {
        Self {
            pkgline,
            arch,
            depend_depth,
            install_time: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            ebin_exposure,
            rdepends: Vec::new(), // Initialize rdepends as empty
            depends: Vec::new(), // Initialize depends as empty
            ebin_links: Vec::new(), // Initialize ebin_links as empty
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct GenerationCommand {
    pub timestamp: String,
    pub action: String,
    pub command_line: String,
    #[serde(default)]
    pub new_packages: Vec<String>,
    #[serde(default)]
    pub del_packages: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct EnvConfig {
    pub name: String,
    pub env_base: String,
    pub env_root: String,

    #[serde(default)]
    pub public: bool,

    #[serde(default)]
    pub register_to_path: bool,
    #[serde(default)]
    pub register_priority: i32,

    #[serde(default)]
    pub env_vars: HashMap<String, String>,

    // Only for importing from exported config file
    #[serde(skip_serializing)]
    #[serde(default)]
    pub packages: HashMap<String, InstalledPackageInfo>,        // key is pkgkey (!= pkgline)
    #[serde(skip_serializing)]
    #[serde(default)]
    pub pypi_packages: HashMap<String, InstalledPackageInfo>,   // key is pkgkey
}

// # ChannelConfig is loaded from ${env_root}/etc/epkg/channel.yaml
// # On `epkg init`, may copy from $EPKG_SRC/channel/${channel}.yaml
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

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct ChannelConfig {
    #[serde(default)]
    pub format: PackageFormat,
    #[serde(default)]
    pub distro: String,
    #[serde(default)]
    pub distro_dirs: Vec<String>,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub versions: Vec<String>,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub repos: HashMap<String, RepoConfig>, // point to online repo, key: repo_name
    pub index_url: String,
    #[serde(default)]
    pub index_url_updates: Option<String>,
    #[serde(default)]
    pub index_url_security: Option<String>,
}

fn default_as_true() -> bool { true }

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct RepoConfig {
    #[serde(default = "default_as_true")]
    pub enabled: bool,
    #[serde(default)]
    pub index_url: Option<String>,
    #[serde(default)]
    pub index_url_updates: Option<String>,
    #[serde(default)]
    pub index_url_security: Option<String>,
    #[serde(default)]
    pub package_baseurl: String, // auto computed from url and ChannelInfo.baseurl
}

static REPODATA_INDICE: LazyLock<std::sync::RwLock<HashMap<String, RepoIndex>>> =
        LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

// Global ENV_CONFIG and CHANNEL_CONFIG using LazyLock
static ENV_CONFIG: LazyLock<EnvConfig> = LazyLock::new(|| {
    crate::io::deserialize_env_config().expect("Failed to deserialize env config")
});

static CHANNEL_CONFIG: LazyLock<ChannelConfig> = LazyLock::new(|| {
    crate::io::deserialize_channel_config().expect("Failed to deserialize channel config")
});

// Accessor functions for global configs
pub fn env_config() -> &'static EnvConfig {
    &ENV_CONFIG
}

pub fn channel_config() -> &'static ChannelConfig {
    &CHANNEL_CONFIG
}

// use at package install time
pub fn repodata_indice() -> std::sync::RwLockReadGuard<'static, HashMap<String, RepoIndex>> {
    REPODATA_INDICE.read().expect("Failed to read repodata index")
}

// use at repo update time
pub fn repodata_indice_mut() -> std::sync::RwLockWriteGuard<'static, HashMap<String, RepoIndex>> {
    REPODATA_INDICE.write().expect("Failed to write repodata index")
}

#[allow(dead_code)]
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
#[allow(dead_code)]
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
    Deinit,
    Init,
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
}

impl From<&str> for EpkgCommand {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "deinit" => EpkgCommand::Deinit,
            "init" => EpkgCommand::Init,
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
            _ => EpkgCommand::None, // Default for empty or unrecognized strings
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct EPKGConfig {
    #[serde(default = "default_common_options")]
    pub common: CommonOptions,
    #[serde(default)]
    pub install: InstallOptions,
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
    pub ignore_missing: bool,

    // N: expire after N seconds
    // 0: never expire (the default)
    // -1: always expire
    #[serde(default)]
    pub metadata_expire: i32,

    #[serde(default)]
    pub proxy: String,

    // Default: 6 parallel download threads
    // If user sets <= 0, it gets adjusted to at least 1 in the implementation
    #[serde(default = "default_nr_parallel")]
    pub nr_parallel: usize,

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

fn default_nr_parallel() -> usize {
    6
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct InstallOptions {
    #[serde(default)]
    pub install_suggests: bool,
    #[serde(default)]
    pub no_install_recommends: bool,
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
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(skip)]
    pub upgrade: bool,
}

pub fn default_version() -> String {
    DEFAULT_VERSION.to_string()
}

#[derive(Debug)]
pub struct EPKGDirs {
    // Base directories
    #[allow(dead_code)]
    pub opt_epkg: PathBuf,
    #[allow(dead_code)]
    pub home_epkg: PathBuf,

    // Subdirectories
    pub home_config: PathBuf,
    pub private_envs: PathBuf,
    pub public_envs: PathBuf,

    // Subdirectories depend on EPKGOptions
    pub epkg_store: PathBuf,
    pub epkg_cache: PathBuf,
    pub epkg_downloads_cache: PathBuf,
}

#[allow(dead_code)]
#[derive(Default)]
pub struct PackageManager {
    // cache need to installing packages info
    pub pkgkey2package: HashMap<String, Arc<Package>>,
    pub pkgline2package: HashMap<String, Arc<Package>>, // cache for locally installed packages

    // loaded from env installed-packages.json
    // `self.installed_packages` (loaded from installed-packages.json) is the
    // authoritative data source. If a pkgkey is not found here, the package
    // is treated as not installed.
    pub installed_packages: HashMap<String, InstalledPackageInfo>, // key is pkgkey (!= pkgline)

    pub has_worker_process: bool,
    pub ipc_socket: String,
    pub ipc_stream: Option<UnixStream>,
    pub child_pid: Option<nix::unistd::Pid>,
}

pub static CLAP_MATCHES: LazyLock<clap::ArgMatches> = LazyLock::new(|| {
    parse_cmdline()
});

static CONFIG: LazyLock<EPKGConfig> = LazyLock::new(|| {
    let matches = &CLAP_MATCHES;
    let config = parse_options_common(&matches)
        .expect("Failed to parse common options for CONFIG");
    parse_options_subcommand(&matches, config)
        .expect("Failed to parse subcommand options for CONFIG")
});

static DIRS: LazyLock<EPKGDirs> = LazyLock::new(|| {
    EPKGDirs::builder()
        .with_options(config().clone())
        .build()
        .expect("Failed to initialize EPKGDirs")
});

pub fn config() -> &'static EPKGConfig {
    &CONFIG
}

pub fn dirs() -> &'static EPKGDirs {
    &DIRS
}
