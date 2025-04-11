use std::process::exit;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{bail, Ok, Result};
use crate::models::*;
use crate::io::load_package_json;
use crate::parse_requires::*;
use crate::paths;

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
                pnames.push(pkgname.clone());
            } else {
                // 输入是真正的pkgname
                pnames.push(capability .clone());
            }
        }

        for pkgname in pnames {
            self.add_one_package_installing(pkgname.as_str(), 0, true, &mut packages, &mut missing_names);
        }

        if !missing_names.is_empty() {
            println!("Missing packages: {:#?}", missing_names);
            if !self.options.ignore_missing {
                exit(1);
            }
        }

        for essential_pkgname in &self.essential_pkgnames {
            self.add_one_package_installing(essential_pkgname.as_str(), 0, false, &mut packages, &mut missing_names);
        }

        packages
    }

    pub fn collect_recursive_depends(&mut self,
        packages: &mut HashMap<String, InstalledPackageInfo>
    ) -> Result<()> {
        let mut depend_packages: HashMap<String, InstalledPackageInfo> = HashMap::new();
        let mut depth = 1;
        let mut repo_format: Option<String> = None;
        if let Some((pkg_hash, _)) = packages.iter().next() {
            let pkg_hash_str = pkg_hash.as_str();
            repo_format = self.pkghash2spec[&pkg_hash_str[0..32]].format.clone();
        }
        
        self.collect_depends(&packages, &mut depend_packages, depth, &repo_format)?;

        while !depend_packages.is_empty() {
            packages.extend(depend_packages);
            depend_packages = HashMap::new();
            depth += 1;
            self.collect_depends(&packages, &mut depend_packages, depth, &repo_format)?;
        }

        Ok(())
    }

    fn load_package_info(&self, pkg_hash: &str) -> Result<Package> {
        let path = format!(
            "{}/channel/{}/{}/{}/pkg-info/{}/{}.json",
            paths::instance.epkg_cache.display(),
            self.env_config.channel.name,
            self.pkghash2spec[&pkg_hash[0..32]].repo,
            self.options.arch,
            &pkg_hash[0..2],
            pkg_hash
        );
        load_package_json(&path).map_err(|e| e.into())
    }

    fn process_dependencies(
        &mut self,
        pkg_info: &Package,
        packages: &HashMap<String, InstalledPackageInfo>,
        depend_packages: &mut HashMap<String, InstalledPackageInfo>,
        depth: u8,
        missing_deps: &mut Vec<String>,
    ) -> Result<()> {
        if let Some(dependencies) = &pkg_info.depends {
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
                    let appbin_flag = spec.source
                        .as_ref()
                        .map_or(false, |s| self.appbin_source.contains(s));
                    
                    depend_packages.insert(
                        dep_id.clone(),
                        InstalledPackageInfo::new(depth, appbin_flag),
                    );
                }
            }
        }
        Ok(())
    }

    fn process_requirements_impl(
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
                    self.process_requirement(
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
    
    fn process_requirement(
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
                        missing_deps.push(format!("{}-{}", capability, pkg_format));
                        return Ok(());
                    }
                };
                let Some(hashes) = self.pkgname2lines.get(pkg_mapping_name) else {
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
    
        if self.options.ignore_missing {
            println!("Missing dependencies ignored: {:?}", missing);
            Ok(())
        } else {
            bail!("Missing dependencies found: {:?}", missing)
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
        for pkg_hash in packages.keys() {
            let pkg_info = self.load_package_info(pkg_hash)?;
            if pkg_info.requires_pre.is_some() {
                let Some(requirements) = &pkg_info.requires_pre else { continue };
                self.process_requirements_impl(
                    requirements,
                    packages,
                    depend_packages,
                    depth,
                    repo_format,
                    &mut missing_deps,
                )?;
            }
            if pkg_info.depends.is_some() {
                self.process_dependencies(
                    &pkg_info,
                    packages,
                    depend_packages,
                    depth,
                    &mut missing_deps,
                )?;
            } else if pkg_info.requires.is_some() {
                let Some(requirements) = &pkg_info.requires else { continue };
                self.process_requirements_impl(
                    requirements,
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
