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
    pub dist: Option<String>,
    pub hash: String,
    pub arch: String,
    #[serde(rename = "sourcePkg")]
    pub source: Option<String>,

    pub summary: Option<String>,
    pub description: Option<String>,

    #[serde(default)]
    pub depends: Option<Vec<Dependency>>,
    pub requires: Option<Vec<String>>,
    pub provides: Option<Vec<String>>,
    pub recommends: Option<Vec<String>>,
    pub suggests: Option<Vec<String>>,
    #[serde(rename = "originUrl")]
    pub origin_url: Option<String>,
    #[serde(rename = "requiresPre")]
    pub requires_pre: Option<Vec<String>>,
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
    #[serde(skip)]
    pub format: Option<String>,
    #[serde(rename = "store-paths")]
    pub store_paths: Vec<StorePathsIndex>,
    #[serde(rename = "pkg-info")]
    pub pkg_infos: Vec<PkgInfoIndex>,
    #[serde(rename = "pkg-files")]
    pub pkg_files: Vec<PkgFilesIndex>,
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
    // pub checksum: String,
    // pub datetime: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct PkgInfoIndex {
    pub filename: String,
    // pub checksum: String,
    // pub datetime: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct PkgFilesIndex {
    pub filename: String,
    // pub checksum: String,
    // pub datetime: String,
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
    pub install_time: u64,
    pub depend_depth: u8,
    pub appbin_flag: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct GenerationCommand {
    pub timestamp: String,
    pub action: String,
    pub new_packages: Vec<String>,
    pub del_packages: Vec<String>,
    pub command_line: String,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct EnvConfig {
    pub name: String,
    pub env_base: String,
    pub env_root: String,

    pub public: bool,

    pub register_to_path: bool,
    pub register_priority: i32,

    pub env_vars: HashMap<String, String>,

    // Only for importing from exported config file
    #[serde(skip_serializing)]
    pub installed_packages: HashMap<String, InstalledPackageInfo>,
}

// # ChannelConfig is loaded from ${env_root}/etc/epkg/channel.yaml
// # On `epkg init`, may copy from $EPKG_SRC/channel/${channel}.yaml
// channel:
//   name: "openeuler:24.03-lts"
//   baseurl: "https://repo.oepkgs.net/openeuler/epkg/channel/openEuler-24.03-LTS/"
//
// repos:
//   everything:
//     # url: defaults to ${channel.baseurl}/$reponame
//   mysql:
//       enabled = false
//       # a repo can specify its own url
//       url = "http://third.party/repo/dir"
#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct ChannelConfig {
    pub name: String,
    pub baseurl: String,
    pub repos: HashMap<String, RepoConfig>,
}

fn default_as_true() -> bool { true }

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
#[derive(Default)]
#[derive(Clone)]
pub struct RepoConfig {
    #[serde(default = "default_as_true")]
    pub enabled: bool,
    pub url: Option<String>,
}

#[allow(dead_code)]
#[derive(Default, Debug, Clone, Deserialize)]
pub struct EPKGConfig {
    pub common: CommonOptions,
    pub install: InstallOptions,
    pub list: ListOptions,
    pub env: EnvOptions,
    pub history: HistoryOptions,
    pub init: InitOptions,

    #[serde(skip)]
    pub config_file: String,
    #[serde(skip)]
    pub command_line: String,
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

    pub quiet: bool,
    pub verbose: bool,
    pub assume_yes: bool,
    pub ignore_missing: bool,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct InstallOptions {
    pub install_suggests: bool,
    pub no_install_recommends: bool,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct ListOptions {
    pub list_all: bool,
    pub list_installed: bool,
    pub list_available: bool,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct EnvOptions {
    pub channel: Option<String>,
    pub priority: Option<i32>,
    pub public: bool,
    pub pure: bool,
    pub stack: bool,

    #[serde(skip)]
    pub env_path: Option<String>,
    #[serde(skip)]
    pub import_file: Option<String>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct HistoryOptions {
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
    "master".to_string()
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
    pub epkg_manager_cache: PathBuf,
}

#[allow(dead_code)]
#[derive(Default)]
pub struct PackageManager {
    pub env_config: HashMap<String, EnvConfig>,
    pub channel_config: HashMap<String, ChannelConfig>,

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
