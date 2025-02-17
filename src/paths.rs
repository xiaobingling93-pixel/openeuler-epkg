use std::fs;
use std::env;
use std::path::PathBuf;

#[allow(dead_code)]
pub struct EPKGPaths {
    pub opt_epkg: PathBuf,
    pub pub_epkg: PathBuf,
    pub home_epkg: PathBuf,
    pub epkg_envs_root: PathBuf,
    pub epkg_config_dir: PathBuf,
    pub epkg_common_root: PathBuf,
    pub epkg_cache: PathBuf,
    pub epkg_store_root: PathBuf,
    pub epkg_pkg_cache_dir: PathBuf,
    pub epkg_channel_cache_dir: PathBuf,
    pub common_profile_link: PathBuf,
    pub elfloader_exec: PathBuf,
    pub epkg_helper_exec: PathBuf,
}

impl EPKGPaths {
    fn new() -> Self {
        let home_dir= env::var("HOME").unwrap();

        let opt_epkg = PathBuf::from("/opt/epkg");
        let pub_epkg = PathBuf::from(format!("{}/users/public", opt_epkg.display()));
        let home_epkg = PathBuf::from(format!("{}/.epkg", home_dir));
        let epkg_envs_root = PathBuf::from(format!("{}/envs", home_epkg.display()));
        let epkg_config_dir = PathBuf::from(format!("{}/config", home_epkg.display()));
        let (epkg_common_root, epkg_cache, epkg_store_root) = if fs::metadata(&pub_epkg).is_ok() {
            (
            PathBuf::from(format!("{}/envs/common", pub_epkg.display())),
            PathBuf::from(format!("{}/cache", opt_epkg.display())),
            PathBuf::from(format!("{}/store", opt_epkg.display())),
            )
        } else {
            (
            PathBuf::from(format!("{}/common", epkg_envs_root.display())),
            PathBuf::from(format!("{}/.cache/epkg", home_dir)),
            PathBuf::from(format!("{}/store", home_epkg.display())),
            )
        };

        let epkg_pkg_cache_dir = PathBuf::from(format!("{}/packages", epkg_cache.display()));
        let epkg_channel_cache_dir = PathBuf::from(format!("{}/channel", epkg_cache.display()));

        let common_profile_link = PathBuf::from(format!("{}/profile-current", epkg_common_root.display()));
        let elfloader_exec = PathBuf::from(format!("{}/usr/bin/elf-loader", common_profile_link.display()));
        let epkg_helper_exec = PathBuf::from(format!("{}/profile-current/usr/bin/epkg-helper", epkg_common_root.display()));

        Self {
            opt_epkg,
            pub_epkg,
            home_epkg,
            epkg_envs_root,
            epkg_config_dir,
            epkg_common_root,
            epkg_cache,
            epkg_store_root,
            epkg_pkg_cache_dir,
            epkg_channel_cache_dir,
            common_profile_link,
            elfloader_exec,
            epkg_helper_exec,
        }
    }
}

lazy_static::lazy_static! {
    pub static ref instance: EPKGPaths = EPKGPaths::new();
}