use std::collections::HashMap;
use std::collections::HashSet;
use std::os::unix::net::UnixStream;
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Dependency {
    pub pkgname: String,
    pub hash: String,
}

// $HOME/.cache/epkg/channel/${channel}/${repo}/${arch}/pkg-info/{2-char-prefix}/${pkghash}__${pkgname}__${pkgver}__${pkgrel}.json
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub release: String,
    // pub epoch: Option<u32>, XXX fix x2epkg output type first, then use this
    pub dist: Option<String>,
    pub hash: String,
    pub arch: String,
    pub source: Option<String>,

    pub summary: Option<String>,
    pub description: Option<String>,

    #[serde(default)]
    pub depends: Vec<Dependency>,
    pub requires: Option<Vec<String>>,
    pub provides: Option<Vec<String>>,
    pub recommends: Option<Vec<String>>,
    pub suggests: Option<Vec<String>>,
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
    #[serde(rename = "store-paths")]
    pub store_paths: Vec<StorePathsIndex>,
    #[serde(rename = "pkg-info")]
    pub pkg_info: Vec<PkgInfoIndex>,
    #[serde(rename = "pkg-files")]
    pub pkg_files: Vec<PkgFilesIndex>,
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
#[derive(Debug)]
pub struct PackageSpec {
    pub repo: String,
    pub hash: String,
    pub name: String,
    pub version: String,
    pub release: String,
    pub source: Option<String>,
}

/*
    # /home/${user}/.epkg/envs/${env}/profile-current/installed-packages.json
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
pub struct ProfileCommand {
    pub timestamp: String,
    pub action: String,
    pub new_packages: Vec<String>,
    pub del_packages: Vec<String>,
    pub command_line: String,
}

// $HOME/.epkg/envs/${env}/profile-current/etc/epkg/channel.yaml
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[derive(Default)]
pub struct EnvConfig {
    pub channel: Channel,
    pub repos: HashMap<String, RepoConfig>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[derive(Default)]
pub struct Channel {
    pub name: String,
    pub baseurl: String,
}

fn default_as_true() -> bool { true }

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    #[serde(default = "default_as_true")]
    pub enabled: bool,
}

#[allow(dead_code)]
#[derive(Default)]
pub struct EPKGOptions {
    // common options
    pub env: String,
    pub arch: String,
    pub download_only: bool,
    pub simulate: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub assume_yes: bool,
    pub ignore_missing: bool,

    // install subcommand options
    pub install_suggests: bool,
    pub no_install_recommends: bool,

    // list subcommand options
    pub list_all: bool,
    pub list_installed: bool,
    pub list_available: bool,
}

#[allow(dead_code)]
#[derive(Default)]
pub struct PackageManager {
    pub options: EPKGOptions,
    pub repos_data: Vec<Repodata>,
    pub env_config: EnvConfig,
    pub appbin_source: HashSet<String>,

    // loaded from repodata.store_paths files
    // pkghash2spec[hash] = PackageSpec
    // pkgname2lines[pkgname] = [pkgline]
    pub pkghash2spec: HashMap<String, PackageSpec>,
    pub pkgname2lines: HashMap<String, Vec<String>>,

    // loaded from env installed-packages.json
    pub installed_packages: HashMap<String, InstalledPackageInfo>,

    pub has_worker_process: bool,
    pub ipc_socket: String,
    pub ipc_stream: Option<UnixStream>,
    pub child_pid: Option<nix::unistd::Pid>,
}
