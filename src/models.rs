use std::collections::HashMap;
use std::collections::HashSet;
use std::os::unix::net::UnixStream;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::LazyLock;
use crate::parse_cmdline;
use crate::parse_options_common;
use crate::parse_options_subcommand;

pub const SUPPORT_ARCH_LIST: &[&str] = &["aarch64", "x86_64", "riscv64", "loongarch64"];
pub const PURE_ENV_SUFFIX: char = '!';
pub const DEFAULT_CHANNEL: &str = &"openeuler:24.03-lts";
pub const DEFAULT_VERSION: &str = &"master"; // epkg init will download this version from gitee

// Package format types
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
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
}

// Mirror configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mirror {
    pub url: String,
    #[serde(default)]
    pub priority: u32,
    #[serde(default)]
    pub top_level: bool,
    #[serde(default)]
    pub supports: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    pub pkgname: String,
    pub hash: String,
}

// $HOME/.cache/epkg/channel/${channel}/${repo}/${arch}/pkg-info/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub release: String,
    // pub epoch: Option<u32>, XXX fix x2epkg output type first, then use this
    #[serde(default)]
    pub dist: Option<String>,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub pkgid: Option<String>, // RPM repodata xml: has pkgid, but no hash
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    #[serde(rename = "sourcePkg")]
    pub source: Option<String>,
    #[serde(default)]
    pub location: Option<String>,

    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub depends: Vec<Dependency>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub recommends: Vec<String>,
    #[serde(default)]
    pub suggests: Vec<String>,
    #[serde(default)]
    #[serde(rename = "originUrl")]
    pub origin_url: Option<String>,
    #[serde(default)]
    #[serde(rename = "requiresPre")]
    pub requires_pre: Vec<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(skip)]
    pub require_caps: Vec<String>,
    #[serde(skip)]
    pub recommend_caps: Vec<String>,
    #[serde(skip)]
    pub suggest_caps: Vec<String>,
}

// $HOME/.cache/epkg/channel/${channel}/${repo}/${arch}/repodata/index.json
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Repodata {
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub dir: String,
    #[serde(default)]
    pub format: Option<String>,

    #[serde(default)]
    #[serde(rename = "store-paths")]
    pub store_paths: Vec<StorePathsIndex>,
    #[serde(default)]
    #[serde(rename = "pkg-info")]
    pub pkg_infos: Vec<FileInfo>,
    #[serde(default)]
    #[serde(rename = "pkg-files")]
    pub pkg_files: Vec<FileInfo>,

    #[serde(skip)]
    pub provide2pkgnames: HashMap<String, Vec<String>>,
    #[serde(skip)]
    pub essential_pkgnames: HashSet<String>,
}

// $HOME/.cache/epkg/channel/${channel}/${repo}/${arch}/repodata/store-paths-{filehash}.txt
// pkgline format: {pkghash}__{pkgname}__{pkgver}__{pkgrel}
// 09c88c8eb9820a3570d9a856b91f419c__libselinux__3.3__5.oe2203sp3
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
#[allow(dead_code)]
pub struct FileInfo {
    pub filename: String,
    pub sha256sum: String,
    pub datetime: String,
    #[allow(dead_code)]
    pub size: u64,
}

// parsed from pkgline
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct PackageSpec {
    pub repo: String,
    pub hash: String,
    pub name: String,
    pub version: String,
    pub release: String,
}

/*
    # ${env_root}/generations/current/installed-packages.json
    {
      "${pkghash1}__${pkgname}__${pkgver}__${pkgrel}": {
        "install_time": xxx,
        "depend_depth": true
      },
      "${pkghash2}__${pkgname}__${pkgver}__${pkgrel}": {
        "install_time": xxx,
      }
    }
*/
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPackageInfo {
    pub depend_depth: u8,
    #[serde(default)]
    pub install_time: u64,
    #[serde(default)]
    pub appbin_flag: bool,
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
    pub packages: HashMap<String, InstalledPackageInfo>,
    #[serde(skip_serializing)]
    #[serde(default)]
    pub pypi_packages: HashMap<String, InstalledPackageInfo>,
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
    pub version: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub versions: Vec<String>,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub repos: HashMap<String, RepoConfig>, // point to online repo, key: repo_name
    #[serde(default)]
    pub repodata_indice: HashMap<String, RepoIndex>, // local repo data, key: repodata_name
    #[serde(default)]
    pub mirrors: Vec<Mirror>,
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

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct RepoIndex {
    pub repo_shards: Vec<RepoShard>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[allow(dead_code)]
pub struct RepoShard {
    #[serde(default)]
    pub packages: FileInfo,
    #[serde(default)]
    pub filelist: Option<FileInfo>,

    #[serde(skip)]
    pub provide2pkgnames: HashMap<String, Vec<String>>,
    #[serde(skip)]
    pub essential_pkgnames: HashSet<String>,
}

#[allow(dead_code)]
#[derive(Default, Debug, Clone, Deserialize)]
pub struct EPKGConfig {
    #[serde(default)]
    pub common: CommonOptions,
    #[serde(default)]
    pub install: InstallOptions,
    #[serde(default)]
    pub list: ListOptions,
    #[serde(default)]
    pub env: EnvOptions,
    #[serde(default)]
    pub history: HistoryOptions,
    #[serde(default)]
    pub init: InitOptions,

    #[serde(skip)]
    pub config_file: String,
    #[serde(skip)]
    pub command_line: String,
    #[serde(skip)]
    pub subcommand: String,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct CommonOptions {
    #[serde(skip)]
    pub env: String,
    #[serde(skip)]
    pub arch: String,
    #[serde(skip)]
    pub download_only: bool,
    #[serde(skip)]
    pub simulate: bool,

    #[serde(default)]
    pub quiet: bool,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub assume_yes: bool,
    #[serde(default)]
    pub ignore_missing: bool,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default = "default_nr_parallel")]
    pub nr_parallel: usize,
    #[serde(default = "default_parallel_processing")]
    pub parallel_processing: bool,
}

fn default_nr_parallel() -> usize {
    6
}

fn default_parallel_processing() -> bool {
    false
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
}

fn default_version() -> String {
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
    pub epkg_pkg_cache: PathBuf,
    pub epkg_channel_cache: PathBuf,
    pub epkg_downloads_cache: PathBuf,
}

#[allow(dead_code)]
#[derive(Default)]
pub struct PackageManager {
    pub envs_config: HashMap<String, EnvConfig>,            // key: env_name
    pub channels_config: HashMap<String, ChannelConfig>,    // key: env_name

    pub repos_data: Vec<Repodata>,
    pub appbin_source: HashSet<String>,
    // loaded from repodata.store_paths files
    // pkghash2spec[hash] = PackageSpec
    // pkgname2lines[pkgname] = [pkgline]
    pub pkghash2spec: HashMap<String, PackageSpec>,
    pub pkgname2lines: HashMap<String, Vec<String>>,
    pub provide2pkgnames: HashMap<String, Vec<String>>,
    pub essential_pkgnames: HashSet<String>,
    // cache need to installing packages info
    pub pkghash2pkg: HashMap<String, Package>,

    // loaded from env installed-packages.json
    pub installed_packages: HashMap<String, InstalledPackageInfo>,

    pub mirrors: HashMap<String, Mirror>,   // key: mirror id

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
    let config = parse_options_common(&matches);
    parse_options_subcommand(&matches, config)
});

static DIRS: LazyLock<EPKGDirs> = LazyLock::new(|| {
    EPKGDirs::builder()
        .with_options(config().clone())
        .build()
        .expect("Failed to initialize EPKGDirs")
});

// 获取全局配置的公共接口
pub fn config() -> &'static EPKGConfig {
    &CONFIG
}

pub fn dirs() -> &'static EPKGDirs {
    &DIRS
}
