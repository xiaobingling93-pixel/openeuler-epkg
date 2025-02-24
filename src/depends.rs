use std::process::exit;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{Result, bail};
use crate::models::*;
use crate::io::load_package_json;
use crate::paths;

impl PackageManager {

    pub fn resolve_appbin_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) {
        for pkgline in packages.keys() {
            if let Some(spec) = self.pkghash2spec.get(&pkgline[0..32]) {
                if let Some(source) = spec.source.clone() {
                    self.appbin_source.insert(source);
                } else {
                    println!("Not get source, pkgline: {:#?}", pkgline);
                }
            }
        }
    }

    /// convert user provided @pkg_names to exact pkglines
    pub fn resolve_package_info(&self, pkg_names: Vec<String>) -> HashMap<String, InstalledPackageInfo> {
        let mut packages = HashMap::new();
        let mut missing_names = Vec::new();

        for pkgname in pkg_names {
            if let Some(pkglines) = self.pkgname2lines.get(&pkgname) {
                for pkgline in pkglines {
                    packages.insert(
                        pkgline.clone(),
                        InstalledPackageInfo {
                            install_time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                            depend_depth: 0,
                        },
                    );
                }
            } else {
                missing_names.push(pkgname);
            }
        }

        if !missing_names.is_empty() {
            println!("Missing packages: {:#?}", missing_names);
            if !self.options.ignore_missing {
                exit(1);
            }
        }

        packages
    }

    pub fn collect_recursive_depends(&mut self,
        packages: &mut HashMap<String, InstalledPackageInfo>
    ) -> Result<()> {
        let mut depend_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut depth = 1;

        self.collect_depends(&packages, &mut depend_packages, depth)?;

        while !depend_packages.is_empty() {
            packages.extend(depend_packages);
            depend_packages = HashMap::new();
            depth += 1;
            self.collect_depends(&packages, &mut depend_packages, depth)?;
        }

        Ok(())
    }

    pub fn collect_depends(&mut self,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
    ) -> Result<()> {
        let mut misses = Vec::new();
        for pkgline in packages.keys() {

            let file_path: String = format!("{}/channel/{}/{}/{}/pkg-info/{}/{}.json",
                paths::instance.epkg_cache.display(),
                self.env_config.channel.name,
                self.pkghash2spec[&pkgline[0..32]].repo,
                self.options.arch,
                &pkgline[0..2],
                pkgline,
            );

            let package = load_package_json(&file_path)?;
            for dep in package.depends {
                if let Some(spec) = self.pkghash2spec.get(&dep.hash) {
                    let dpkgline = format!("{}__{}__{}__{}",
                        spec.hash,
                        spec.name,
                        spec.version,
                        spec.release);
                    if !packages.contains_key(&dpkgline) &&
                        !depend_packages.contains_key(&dpkgline)
                    {
                        depend_packages.insert(
                            dpkgline.clone(),
                            InstalledPackageInfo {
                                install_time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                                depend_depth: depth,
                            },
                        );
                    }
                } else {
                    misses.push(format!("{}-{}", dep.pkgname, dep.hash));
                }
            }
        }

        if !misses.is_empty() {
            if !self.options.ignore_missing {
                bail!("Missing dependency: {:?}", misses); // Return an error
            } else {
                println!("Cannot find depends for packages: {:?}", misses);
            }
        }

        Ok(())
    }

}
