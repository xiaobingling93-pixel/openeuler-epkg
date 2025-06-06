use std::process::exit;
use std::collections::{HashMap};
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::Arc;
use color_eyre::Result;
use color_eyre::eyre;
use log;
use crate::models::*;

use crate::parse_requires::*;

impl InstalledPackageInfo {
    fn new(depth: u8, appbin_flag: bool) -> Self {
        Self {
            pkgline: String::new(),
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
        log::debug!("Recording appbin source for {} packages", packages.len());
        for pkgkey in packages.keys() {
            let pkg_json = self.load_package_info(pkgkey)?;
            if pkg_json.source.is_some() {
                self.appbin_source.insert(pkg_json.source.as_ref().unwrap().clone());
            }
        }
        Ok(())
    }

    pub fn change_appbin_flag_same_source(&mut self, packages: &mut HashMap<String, InstalledPackageInfo>) -> Result<()> {
        log::debug!("Checking appbin flag for {} packages with same source", packages.len());
        for (pkgkey, package_info) in packages.iter_mut() {
            let pkg_json = self.load_package_info(pkgkey.as_str())?;
            if package_info.appbin_flag == false && pkg_json.source.is_some() {
                let Some(source) = &pkg_json.source else { continue };
                if self.appbin_source.contains(source) {
                    package_info.appbin_flag = true;
                }
            }
        }
        Ok(())
    }

    fn add_one_package_installing(&mut self, pkg_name: &str, depth: u8, ebin_flag: bool,
                                  packages: &mut HashMap<String, InstalledPackageInfo>,
                                  missing_names: &mut Vec<String>) {
        log::debug!("Adding package '{}' with depth {} and ebin_flag {}", pkg_name, depth, ebin_flag);
        match self.map_pkgname2packages(pkg_name) {
            Ok(packages_list) => {
                for package in packages_list {
                    packages.insert(
                        package.pkgkey.clone(),
                        InstalledPackageInfo::new(depth, ebin_flag),
                    );
                }
            },
            Err(_) => {
                missing_names.push(pkg_name.to_string());
            }
        }
    }

    /// convert user provided @capabilities to exact packages hash
    pub fn resolve_package_info(&mut self, capabilities: Vec<String>) -> HashMap<String, InstalledPackageInfo> {
        log::debug!("Resolving package info for {} capabilities", capabilities.len());
        log::trace!("Capabilities: {:?}", capabilities);
        let mut packages = HashMap::new();
        let mut missing_names = Vec::new();
        let mut pnames = Vec::new();

        for capability  in capabilities {
            match crate::mmio::map_provide2pkgnames(capability.as_str()) {
                Ok(pkgnames) if !pkgnames.is_empty() => {
                    pnames.push(pkgnames[0].clone());
                },
                _ => {
                    // 输入是真正的pkgname
                    pnames.push(capability.clone());
                }
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
        log::debug!("Collecting essential packages");
        let mut missing_names = Vec::new();
        let essential_pkgnames = crate::mmio::get_essential_pkgnames()?;
        for essential_pkgname in &essential_pkgnames {
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
        log::debug!("Starting recursive dependency collection for {} packages", packages.len());
        let mut depend_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut depth = 1;
        let channel_config = self.get_channel_config(config().common.env.clone())?;
        let repo_format = channel_config.format;

        self.collect_depends(&packages, &mut depend_packages, depth, repo_format)?;

        while !depend_packages.is_empty() {
            log::debug!("Found {} new dependencies at depth {}", depend_packages.len(), depth);
            packages.extend(depend_packages);
            depend_packages = HashMap::new();
            depth += 1;
            self.collect_depends(&packages, &mut depend_packages, depth, repo_format)?;
        }

        Ok(())
    }

    fn process_dependencies(
        &mut self,
        dependencies: &Vec<Dependency>,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        log::trace!("Dependencies: {:?}", dependencies);
        for dep in dependencies {
            let pkgkey = crate::mmio::format_pkgkey(&dep.pkgname, &dep.ca_hash);

            if !packages.contains_key(&pkgkey) &&
                !depend_packages.contains_key(&pkgkey) {
                match self.load_package_info(&pkgkey) {
                    Ok(_package) => {
                        depend_packages.insert(
                            pkgkey,
                            InstalledPackageInfo::new(depth, false),
                        );
                    }
                    Err(_) => {
                        missing_deps.push(pkgkey);
                    }
                }
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
        repo_format: PackageFormat,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        log::trace!("Depth: {} Requirements: {:?}", depth, requirements);
        for req in requirements {
            let and_deps = match parse_requires(repo_format, req) {
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

    // Filter packages based on architecture that matches config().common.arch
    fn filter_packages_by_arch(&self, packages: Vec<Package>) -> Vec<Package> {
        let target_arch = crate::models::config().common.arch.as_str();

        // If there are no packages with matching architecture, return all packages
        let arch_packages: Vec<Package> = packages.iter()
            .filter(|pkg| !pkg.arch.is_empty() && pkg.arch == target_arch)
            .cloned()
            .collect();

        log::trace!(
            "Filtered packages by architecture '{}': {} out of {} packages matched",
            target_arch,
            arch_packages.len(),
            packages.len()
        );

        if !arch_packages.is_empty() {
            arch_packages
        } else {
            packages
        }
    }

    fn process_requirement_impl(
        &mut self,
        capability: &str,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        // First try to find packages by capability name directly
        let unfiltered_packages = match self.map_pkgname2packages(capability) {
            Ok(packages_list) if !packages_list.is_empty() => packages_list,
            _ => {
                // If not found, try to resolve through provides mapping
                let pkg_mapping_names = match crate::mmio::map_provide2pkgnames(capability) {
                    Ok(pkgnames) if !pkgnames.is_empty() => pkgnames,
                    _ => {
                        if !capability.starts_with("rpmlib(") {
                            log::warn!("Missing capability: {}", capability);
                            missing_deps.push(capability.to_string());
                        } else {
                            log::trace!("Ignoring rpmlib capability: {}", capability);
                        }
                        return Ok(());
                    }
                };
                match self.map_pkgname2packages(&pkg_mapping_names[0]) {
                    Ok(packages_list) if !packages_list.is_empty() => packages_list,
                    _ => {
                        log::warn!("Missing mapped capability: {}", capability);
                        missing_deps.push(format!("{}", capability));
                        return Ok(());
                    }
                }
            }
        };

        // Filter packages by architecture
        let pkg_packages = self.filter_packages_by_arch(unfiltered_packages);

        for package in pkg_packages {
            let pkgkey = &package.pkgkey;
            if !packages.contains_key(pkgkey) && !depend_packages.contains_key(pkgkey) {
                depend_packages.insert(
                    pkgkey.clone(),
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
        repo_format: PackageFormat,
    ) -> Result<()> {
        log::debug!("Collecting dependencies for {} packages at depth {}", packages.len(), depth);
        let mut missing_deps = Vec::new();
        for pkgkey in packages.keys() {
            let pkg_info = self.load_package_info(pkgkey)?;

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


    pub fn map_pkgname2packages(&mut self, pkgname: &str) -> Result<Vec<Package>> {
        match crate::mmio::map_pkgname2packages(pkgname) {
            Ok(packages_list) => {
                for package in &packages_list {
                    // cache for later references
                    log::trace!("Caching package: {}", package.pkgkey);
                    self.pkgkey2package.insert(package.pkgkey.clone(), Arc::new(package.clone()));
                }
                return Ok(packages_list);
            },
            Err(e) => Err(e)
        }
    }

    pub fn map_pkgline2package(&mut self, pkgline: &str) -> Result<Arc<Package>> {
        // Check cache first
        if let Some(package) = self.pkgline2package.get(pkgline) {
            log::trace!("Found cached package info for pkgline '{}'", pkgline);
            return Ok(Arc::clone(package));
        }

        // Load from mmio function
        match crate::mmio::map_pkgline2package(pkgline) {
            Ok(package) => {
                log::trace!("Caching package from pkgline: {}", pkgline);
                let arc_package = Arc::new(package);
                self.pkgline2package.insert(pkgline.to_string(), Arc::clone(&arc_package));
                Ok(arc_package)
            },
            Err(e) => Err(e)
        }
    }

    pub fn load_package_info(&mut self, pkgkey: &str) -> Result<Arc<Package>> {
        log::trace!("Loading package info for '{}'", pkgkey);
        // Try to find by pkgkey first
        if let Some(package) = self.pkgkey2package.get(pkgkey) {
            log::trace!("Found cached package info for '{}'", pkgkey);
            return Ok(Arc::clone(package));
        }

        // Extract package name from pkgkey and try to load all packages with that name
        log::debug!("Package '{}' not in cache, extracting package name", pkgkey);
        let pkgname = crate::mmio::pkgkey2pkgname(pkgkey)?;
        self.map_pkgname2packages(&pkgname)?;

        // Try to find the package again after loading
        if let Some(package) = self.pkgkey2package.get(pkgkey) {
            log::debug!("Found package '{}' after loading", pkgkey);
            return Ok(Arc::clone(package));
        }

        log::warn!("Package not found: {}", pkgkey);
        Err(eyre::eyre!("Package not found: {}", pkgkey))
    }

}
