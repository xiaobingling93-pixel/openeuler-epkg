use std::env;
use std::collections::HashMap;
use chrono::Utc;
use anyhow::{Result, bail};
use crate::models::*;
use crate::io::load_package_json;

impl PackageManager {

    pub fn collect_depends(&mut self,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
    ) -> Result<()> {
        let mut misses = Vec::new();
        for pkgline in packages.keys() {

            let file_path: String = format!("{}/.cache/epkg/channel/{}/{}/{}/pkg-info/{}/{}.json",
                env::var("HOME")?,
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
                    if !self.installed_packages.contains_key(&dpkgline)
                        && !packages.contains_key(&dpkgline)
                            && !depend_packages.contains_key(&dpkgline)
                    {
                        depend_packages.insert(
                            dpkgline.clone(),
                            InstalledPackageInfo {
                                install_time: Utc::now(),
                                manual_install: false,
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
