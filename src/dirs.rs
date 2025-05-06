use std::env;
use std::path::PathBuf;
use std::io::{self, ErrorKind};
use crate::models::EPKGOptions;
use crate::models::EPKGDirs;

#[derive(Default)]
pub struct EPKGDirsBuilder {
    options: Option<EPKGOptions>,
    custom_home: Option<PathBuf>,
    custom_opt: Option<PathBuf>,
}

impl EPKGDirs {
    pub fn builder() -> EPKGDirsBuilder {
        EPKGDirsBuilder::default()
    }

    // Helper method to create dirs using proper path joining
    fn build_dirs(options: &EPKGOptions, home_epkg: &PathBuf, opt_epkg: &PathBuf) -> io::Result<Self> {
        let (store_root, cache_root) = if options.shared_store {
            (opt_epkg.join("store"), opt_epkg.join("cache"))
        } else {
            (home_epkg.join("store"), get_xdg_cache()?.join("epkg"))
        };

        // Get username - if it fails, the error will propagate upward
        let username = get_username()?;
        let user_envs = opt_epkg.join(format!("users/{}/envs", username));

        Ok(Self {
            opt_epkg: opt_epkg.clone(),
            home_epkg: home_epkg.clone(),
            home_config: home_epkg.join("config"),
            private_envs: home_epkg.join("envs"),
            public_envs: user_envs,
            epkg_store: store_root,
            epkg_cache: cache_root.clone(),
            epkg_pkg_cache: cache_root.join("packages"),
            epkg_channel_cache: cache_root.join("channel"),
            epkg_manager_cache: cache_root.join("manager"),
        })
    }
}

impl EPKGDirsBuilder {
    pub fn with_options(mut self, options: EPKGOptions) -> Self {
        self.options = Some(options);
        self
    }

    pub fn with_custom_home(mut self, path: PathBuf) -> Self {
        self.custom_home = Some(path);
        self
    }

    pub fn build(self) -> io::Result<EPKGDirs> {
        let options = self.options.unwrap_or_default();

        let home_epkg = match self.custom_home {
            Some(path) => path,
            None => {
                let home = env::var("HOME")
                    .map_err(|_| io::Error::new(ErrorKind::NotFound, "HOME environment variable not found"))?;
                PathBuf::from(home).join(".epkg")
            }
        };

        let opt_epkg = self.custom_opt.unwrap_or_else(|| PathBuf::from("/opt/epkg"));

        EPKGDirs::build_dirs(&options, &home_epkg, &opt_epkg)
    }
}

impl PackageManager {

    pub fn get_env_root(&mut self, env_name: String) -> Result<PathBuf> {
        let env_config = self.get_env_config(env_name)?;
        Ok(PathBuf::from(&env_config.env_root))
    }

    pub fn get_default_env_root(&mut self) -> Result<PathBuf> {
        self.get_env_root(self.options.env.clone())
    }

    pub fn get_generations_root(&mut self, env_name: &str) -> Result<PathBuf> {
        let env_root = self.get_env_root(env_name.to_string())?;
        Ok(env_root.join("generations"))
    }

    pub fn get_default_generations_root(&mut self) -> Result<PathBuf> {
        self.get_generations_root(self.options.env.clone())
    }

}

fn get_xdg_cache() -> io::Result<PathBuf> {
    env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME")
            .map(|home| PathBuf::from(home).join(".cache")))
        .map_err(|_| io::Error::new(ErrorKind::NotFound, "Could not determine cache directory"))
}

fn get_username() -> io::Result<String> {
    // Try USER environment variable first (common on Unix/Linux)
    if let Ok(username) = env::var("USER") {
        if !username.is_empty() {
            return Ok(username);
        }
    }

    // Try USERNAME environment variable (common on Windows)
    if let Ok(username) = env::var("USERNAME") {
        if !username.is_empty() {
            return Ok(username);
        }
    }

    // Try to get username from HOME path
    if let Ok(home) = env::var("HOME") {
        let path = PathBuf::from(home);
        if let Some(username) = path.file_name().and_then(|n| n.to_str()) {
            if !username.is_empty() {
                return Ok(username.to_string());
            }
        }
    }

    // If all else fails, return a descriptive error
    Err(io::Error::new(
        ErrorKind::NotFound,
        "Could not determine username. Please ensure either USER or USERNAME environment variables are set. \
         This is required to set up the public environments directory."
    ))
}
