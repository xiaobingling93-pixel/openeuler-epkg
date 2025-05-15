use std::process::exit;
use std::collections::{HashMap};
use std::time::{SystemTime, UNIX_EPOCH};
use color_eyre::Result;
use color_eyre::eyre;
use crate::models::*;
use crate::io::load_package_json;
use crate::parse_requires::*;

impl InstalledPackageInfo {
    fn new(depth: u8, appbin_flag: bool) -> Self {
        Self {
            install_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            depend_depth: depth,
            appbin_flag,
        }
    }
}

impl PackageManager {
    pub fn record_appbin_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<()> {
        let mut tmp_format: Option<String> = None;
        for pkgline in packages.keys() {
            let pkg_json = self.load_package_info(pkgline)?;
            if pkg_json.source.is_some() {
                self.appbin_source.insert(pkg_json.source.as_ref().unwrap().clone());
            }
            if tmp_format.is_none() {
                tmp_format = match pkg_json.origin_url {
                    Some(ref url) => {
                        get_package_format(url)
                    },
                    None => {
                        Some("rpm".to_string())
                    }
                };
            }
        }
        self.repos_data[0].format = tmp_format;
        Ok(())
    }

    pub fn change_appbin_flag_same_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<()> {
        for (pkgline, package_info) in packages.iter_mut() {
            let pkg_json = self.load_package_info(pkgline.as_str())?;
            if package_info.appbin_flag == false && pkg_json.source.is_some() {
                let Some(source) = &pkg_json.source else { continue };
                if self.appbin_source.contains(source) {
                    package_info.appbin_flag = true;
                }
            }
        }
        Ok(())
    }

    fn add_one_package_installing(&self, pkg_name: &str, depth: u8, ebin_flag: bool,
                                  packages: &mut HashMap<String, InstalledPackageInfo>,
                                  missing_names: &mut Vec<String>) {
        if let Some(pkglines) = self.pkgname2lines.get(pkg_name) {
            for pkgline in pkglines {
                packages.insert(
                    pkgline.clone(),
                    InstalledPackageInfo::new(depth, ebin_flag),
                );
            }
        } else {
            missing_names.push(pkg_name.to_string());
        }
    }

    /// convert user provided @capabilities to exact packages hash
    pub fn resolve_package_info(&self, capabilities: Vec<String>) -> HashMap<String, InstalledPackageInfo> {
        let mut packages = HashMap::new();
        let mut missing_names = Vec::new();
        let mut pnames = Vec::new();

        for capability  in capabilities {
            if let Some(pkgname) = self.provide2pkgnames.get(capability .as_str()) {
                pnames.push(pkgname[0].clone());
            } else {
                // 输入是真正的pkgname
                pnames.push(capability .clone());
            }
        }

        for pkgname in pnames {
            self.add_one_package_installing(pkgname.as_str(), 0, true, &mut packages, &mut missing_names);
        }

        if !missing_names.is_empty() {
            eprintln!("Missing packages: {:#?}", missing_names);
            if !config().common.ignore_missing {
                exit(1);
            }
        }

        packages
    }

    pub fn collect_essential_packages(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<()> {
        let mut missing_names = Vec::new();
        for essential_pkgname in &self.essential_pkgnames {
            self.add_one_package_installing(essential_pkgname.as_str(), 0, false, packages, &mut missing_names);
        }
        if !missing_names.is_empty() {
            println!("Missing packages: {:#?}", missing_names);
            if !config().common.ignore_missing {
                exit(1);
            }
        }

        Ok(())
    }

    pub fn collect_recursive_depends(&mut self,
        packages: &mut HashMap<String, InstalledPackageInfo>
    ) -> Result<()> {
        let mut depend_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut depth = 1;
        let repo_format: Option<String> = self.repos_data[0].format.clone();

        self.collect_depends(&packages, &mut depend_packages, depth, &repo_format)?;

        while !depend_packages.is_empty() {
            packages.extend(depend_packages);
            depend_packages = HashMap::new();
            depth += 1;
            self.collect_depends(&packages, &mut depend_packages, depth, &repo_format)?;
        }

        Ok(())
    }

    fn load_package_info(&mut self, pkgline: &str) -> Result<Package> {
        if let Some(package) = self.pkghash2pkg.get(&pkgline[0..32]) {
            return Ok(package.clone());
        }

        let spec = self.pkghash2spec.get(&pkgline[0..32])
            .cloned()
            .ok_or_else(|| eyre::eyre!("Package spec not found"))?;

        let channel_config = self.get_channel_config(config().common.env.clone())?;
        let path = format!(
            "{}/channel/{}/{}/{}/pkg-info/{}/{}.json",
            dirs().epkg_cache.display(),
            channel_config.channel,
            spec.repo,
            config().common.arch,
            &pkgline[0..2],
            pkgline
        );
        let package = load_package_json(&path)?;
        self.pkghash2pkg.insert(pkgline.to_string(), package.clone());
        Ok(package)
    }

    fn process_dependencies(
        &mut self,
        dependencies: &Vec<Dependency>,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        for dep in dependencies {
            let Some(spec) = self.pkghash2spec.get(&dep.hash) else {
                missing_deps.push(format!("{}-{}", dep.pkgname, dep.hash));
                continue;
            };

            let dep_id = format!(
                "{}__{}__{}__{}",
                spec.hash, spec.name, spec.version, spec.release
            );

            if !packages.contains_key(&dep_id) && !depend_packages.contains_key(&dep_id) {
                depend_packages.insert(
                    dep_id.clone(),
                    InstalledPackageInfo::new(depth, false),
                );
            }
        }

        Ok(())
    }

    fn process_requirements(
        &mut self,
        requirements: &Vec<String>,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        repo_format: &Option<String>,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        let pkg_format = match repo_format {
            Some(format) => format,
            None => {
                // [TODO] 稳定后这里应该return Err
                return Ok(());
            }
        };

        for req in requirements {
            let and_deps = match parse_requires(&pkg_format, req) {
                std::result::Result::Ok(deps) => deps,
                Err(e) => {
                    missing_deps.push(format!("Failed to parse requirement '{}': {}", req, e));
                    continue;
                }
            };
            for or_depends in and_deps {
                for pkg_depend in or_depends {
                    self.process_requirement_impl(
                        &pkg_depend.capability,
                        pkg_format.as_str(),
                        packages,
                        depend_packages,
                        depth,
                        missing_deps,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn process_requirement_impl(
        &mut self,
        capability: &str,
        pkg_format: &str,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        let pkg_hashes = match self.pkgname2lines.get(capability) {
            Some(hash) => hash,
            None => {
                let pkg_mapping_name = match self.provide2pkgnames.get(capability) {
                    Some(pkg_name) => pkg_name,
                    None => {
                        if !capability.starts_with("rpmlib(") {
                            missing_deps.push(format!("{}-{}", capability, pkg_format));
                        }
                        return Ok(());
                    }
                };
                let Some(hashes) = self.pkgname2lines.get(pkg_mapping_name[0].as_str()) else {
                    missing_deps.push(format!("{}-{}", capability, pkg_format));
                    return Ok(());
                };
                hashes
            }
        };

        for hash in pkg_hashes {
            if !packages.contains_key(hash) && !depend_packages.contains_key(hash) {
                depend_packages.insert(
                    hash.clone(),
                    InstalledPackageInfo::new(depth, false),
                );
            }
        }
        Ok(())
    }

    fn handle_missing_dependencies(
        &self,
        missing: Vec<String>
    ) -> Result<()> {
        if missing.is_empty() {
            return Ok(());
        }

        if config().common.ignore_missing {
            println!("Missing dependencies ignored: {:?}", missing);
            Ok(())
        } else {
            eyre::bail!("Missing dependencies found: {:?}", missing)
        }
    }

    pub fn collect_depends(
        &mut self,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        repo_format: &Option<String>,
    ) -> Result<()> {
        let mut missing_deps = Vec::new();
        for pkgline in packages.keys() {
            let pkg_info = self.load_package_info(pkgline)?;

            if !pkg_info.requires_pre.is_empty() {
                self.process_requirements(
                    &pkg_info.requires_pre,
                    packages,
                    depend_packages,
                    depth,
                    repo_format,
                    &mut missing_deps,
                )?;
            }
            if !pkg_info.depends.is_empty() {
                self.process_dependencies(
                    &pkg_info.depends,
                    packages,
                    depend_packages,
                    depth,
                    &mut missing_deps,
                )?;
            } else if !pkg_info.requires.is_empty() {
                self.process_requirements(
                    &pkg_info.requires,
                    packages,
                    depend_packages,
                    depth,
                    repo_format,
                    &mut missing_deps,
                )?;
            }
        }

        self.handle_missing_dependencies(missing_deps)?;
        Ok(())
    }

}
