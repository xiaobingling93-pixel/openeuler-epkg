use std::collections::HashMap;
use std::fs;
use color_eyre::Result;
use serde_json::{Value, json};
use crate::models::Package;
use crate::models::dirs;
use crate::models::PACKAGE_CACHE;

pub fn show_package_info(
    all_args: &[String],
    show_files: bool,
    show_scripts: bool,
    show_store_path: bool,
) -> Result<()> {
    // Separate package specs from key=val filters
    let (package_specs, filters) = parse_args_and_filters(all_args);

    if package_specs.is_empty() {
        println!("No package specifications provided");
        return Ok(());
    }

    // Process each package specification
    for package_spec in package_specs {
        process_package_spec(
            &package_spec,
            &filters,
            show_store_path,
            show_scripts,
            show_files,
        )?;
    }

    Ok(())
}

fn parse_args_and_filters(all_args: &[String]) -> (Vec<String>, HashMap<String, String>) {
    let mut package_specs = Vec::new();
    let mut filters = HashMap::new();

    // Valid filter keys that can be used with key=val syntax
    // Examples: version=1.0, arch=x86_64, summary="Some package"
    let valid_filter_keys = ["version", "arch", "summary", "maintainer", "section", "priority", "homepage"];

    for arg in all_args {
        if let Some(equals_pos) = arg.find('=') {
            // Check if the '=' is inside parentheses (e.g., font(:lang=en))
            // This handles provides/capabilities with parameters like:
            //   - font(:lang=en) -> package spec
            //   - gstreamer1(decoder-video/x-dv)(systemstream=true) -> package spec
            let before_equals = &arg[..equals_pos];
            let mut paren_depth = 0;
            for ch in before_equals.chars() {
                if ch == '(' {
                    paren_depth += 1;
                } else if ch == ')' {
                    paren_depth -= 1;
                }
            }
            let is_inside_parens = paren_depth > 0;

            // If '=' is inside parentheses, treat as package spec
            // Examples:
            //   - font(:lang=en) -> package spec (provide with parameter)
            //   - libfoo(x86-64)=2.0 -> package spec (provide with arch and version)
            if is_inside_parens {
                package_specs.push(arg.clone());
                continue;
            }

            // Check if it's a valid filter key
            // Examples:
            //   - version=1.0 -> filter
            //   - arch=x86_64 -> filter
            //   - pkgname=1.0 -> package spec (not a valid filter key)
            if let Some((key, val)) = arg.split_once('=') {
                if valid_filter_keys.contains(&key) {
                    filters.insert(key.to_string(), val.to_string());
                } else {
                    // Not a valid filter key, treat as package spec
                    // Examples:
                    //   - pkgname=1.0 -> package spec (version constraint)
                    //   - mypackage=2.5 -> package spec
                    package_specs.push(arg.clone());
                }
            } else {
                package_specs.push(arg.clone());
            }
        } else {
            // No '=' sign, definitely a package spec
            // Examples:
            //   - font -> package spec (package name)
            //   - libfoo -> package spec (package name)
            package_specs.push(arg.clone());
        }
    }

    (package_specs, filters)
}

fn process_package_spec(
    package_spec: &str,
    filters: &HashMap<String, String>,
    show_store_path: bool,
    show_scripts: bool,
    show_files: bool,
) -> Result<()> {
    // Check if package_spec looks like a pkgkey (format: pkgname__version__arch)
    // A pkgkey has exactly 2 '__' separators, resulting in 3 parts when split
    let parts: Vec<&str> = package_spec.split("__").collect();
    let is_pkgkey = parts.len() == 3 && !parts[0].is_empty() && !parts[1].is_empty() && !parts[2].is_empty();

    let mut packages = Vec::new();

    if is_pkgkey {
        // Try to look up by pkgkey directly - only show the exact matching package
        match crate::mmio::map_pkgkey2package(package_spec) {
            Ok(package) => {
                packages.push(package);
            }
            Err(_) => {
                // If pkgkey lookup fails, don't fall back - show error for exact match
                println!("No packages found matching '{}'", package_spec);
                return Ok(());
            }
        }
    } else {
        // Get packages from repository by pkgname
        packages = crate::package_cache::map_pkgname2packages(package_spec)?;

        // If no packages found, retry with capability/provide mapping
        if packages.is_empty() {
            // Try to find provider package names for this capability
            let provider_pkgnames = crate::mmio::map_provide2pkgnames(package_spec)?;

            // For each provider package name, get its packages
            for provider_pkgname in provider_pkgnames {
                let mut provider_packages = crate::package_cache::map_pkgname2packages(&provider_pkgname)?;
                packages.append(&mut provider_packages);
            }
        }
    }

    // Apply key=val filtering if provided
    if !filters.is_empty() {
        packages.retain(|pkg| apply_filters(pkg, filters));
    }

    if packages.is_empty() {
        println!("No packages found matching '{}'", package_spec);
        return Ok(());
    }

    // Process each matching package
    for package in packages {
        process_single_package(
            &package,
            show_store_path,
            show_scripts,
            show_files,
        )?;
    }

    Ok(())
}

fn apply_filters(package: &Package, filters: &HashMap<String, String>) -> bool {
    for (key, expected_value) in filters {
        let actual_value = match key.as_str() {
            "version" => &package.version,
            "arch" => &package.arch,
            "summary" => &package.summary,
            "maintainer" => &package.maintainer,
            "section" => package.section.as_deref().unwrap_or(""),
            "priority" => package.priority.as_deref().unwrap_or(""),
            "homepage" => &package.homepage,
            _ => "",
        };
        if actual_value != expected_value {
            return false;
        }
    }
    true
}

fn process_single_package(
    package: &Package,
    show_store_path: bool,
    show_scripts: bool,
    show_files: bool,
) -> Result<()> {
    let is_installed = PACKAGE_CACHE.installed_packages.read().unwrap().contains_key(&package.pkgkey);

    if show_store_path {
        show_store_path_info(package, is_installed)?;
        return Ok(());
    }

    if show_scripts {
        show_scripts_info(package, is_installed)?;
        return Ok(());
    }

    if show_files {
        show_files_info(package, is_installed)?;
        return Ok(());
    }

    // Default: show comprehensive package info
    show_comprehensive_info(package, is_installed)?;
    Ok(())
}

fn show_store_path_info(
    package: &Package,
    is_installed: bool,
) -> Result<()> {
    if is_installed {
        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Some(installed_info) = installed.get(&package.pkgkey) {
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            println!("{}", store_path.display());
        }
    } else {
        println!("Package {} is not installed", package.pkgkey);
    }
    Ok(())
}

fn show_scripts_info(
    package: &Package,
    is_installed: bool,
) -> Result<()> {
    if !is_installed {
        println!("Package {} is not installed", package.pkgkey);
        return Ok(());
    }

    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    if let Some(installed_info) = installed.get(&package.pkgkey) {
        let scripts_path = dirs().epkg_store
            .join(&installed_info.pkgline)
            .join("info/install");

        if scripts_path.exists() {
            if let Ok(entries) = fs::read_dir(&scripts_path) {
                // Collect entries into a Vec to avoid consuming the iterator
                let entries: Vec<_> = entries.collect();

                if !entries.is_empty() {
                    println!("Install scriptlets for {}:", package.pkgkey);
                }

                for entry_result in entries {
                    if let Ok(entry) = entry_result {
                        let file_path = entry.path();
                        if file_path.is_file() {
                            println!("=== {} ===", file_path.file_name().unwrap().to_string_lossy());
                            if let Ok(content) = fs::read_to_string(&file_path) {
                                println!("{}", content);
                            }
                            println!();
                        }
                    }
                }
            }
        } else {
            println!("No install scripts found for {}", package.pkgkey);
        }
    }
    Ok(())
}

fn show_files_info(
    package: &Package,
    is_installed: bool,
) -> Result<()> {
    if !is_installed {
        println!("Package {} is not installed", package.pkgkey);
        return Ok(());
    }

    let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
    if let Some(installed_info) = installed.get(&package.pkgkey) {
        let filelist_path = dirs().epkg_store
            .join(&installed_info.pkgline)
            .join("info/filelist.txt");

        if filelist_path.exists() {
            println!("Files for {}:", package.pkgkey);
            if let Ok(content) = fs::read_to_string(&filelist_path) {
                print!("{}", content);
            }
        } else {
            println!("No filelist found for {}", package.pkgkey);
        }
    }
    Ok(())
}

/// Convert a Package struct to a vector of (field_name, field_value) pairs
fn package_to_fields(package: &Package) -> std::collections::HashMap<String, String> {
    let mut package_fields = std::collections::HashMap::new();

    // Convert package to JSON Value to iterate over its fields
    let package_json = json!(package);

    if let Value::Object(map) = package_json {
        // Iterate over all fields in the package
        for (key, value) in map.iter() {
            // Skip null values and empty arrays/objects
            if value.is_null() ||
               (value.is_array() && value.as_array().unwrap().is_empty()) ||
               (value.is_object() && value.as_object().unwrap().is_empty()) {
                continue;
            }

            // Format the value based on its type
            let formatted_value = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Array(arr) => {
                    // Handle arrays (like requires, provides, etc.)
                    let strings: Vec<String> = arr.iter()
                        .filter_map(|v| {
                            if v.is_string() {
                                Some(v.as_str().unwrap().to_string())
                            } else {
                                // For objects in arrays (like dependencies), convert to string
                                Some(v.to_string())
                            }
                        })
                        .collect();
                    strings.join(", ")
                },
                Value::Object(_) => value.to_string(), // Convert objects to string
                Value::Null => continue, // Skip null values
            };

            // Skip empty strings
            if formatted_value.is_empty() {
                continue;
            }

            package_fields.insert(key.clone(), formatted_value);
        }
    }

    package_fields
}

/// Add installation status and related fields to the package fields
fn add_installation_info(
    package_fields: &mut std::collections::HashMap<String, String>,
    package: &Package,
    is_installed: bool,
) {
    if is_installed {
        package_fields.insert("status".to_string(), "Installed".to_string());

        let installed = PACKAGE_CACHE.installed_packages.read().unwrap();
        if let Some(installed_info) = installed.get(&package.pkgkey) {
            let store_path = dirs().epkg_store.join(&installed_info.pkgline);
            package_fields.insert("storePath".to_string(), store_path.display().to_string());

            // Add specific fields from installed_info
            package_fields.insert("dependDepth".to_string(), installed_info.depend_depth.to_string());
            package_fields.insert("installTime".to_string(), installed_info.install_time.to_string());
            if installed_info.ebin_exposure {
                package_fields.insert("ebin".to_string(), "true".to_string());
            }

            // Try to load additional package info from store
            // let package_txt_path = store_path.join("info/package.txt");

            // if package_txt_path.exists() {
            //     if let Ok(local_package) = crate::mmio::map_pkgline2package(&installed_info.pkgline) {
            //         // Ensure critical fields are always included
            //         if let Some(ca_hash) = &local_package.ca_hash {
            //             package_fields.insert("caHash".to_string(), ca_hash.clone());
            //         }
            //     }
            // }
        }
    } else {
        package_fields.insert("status".to_string(), "Available".to_string());
    }
}

/// Show comprehensive information about a package
fn show_comprehensive_info(
    package: &Package,
    is_installed: bool,
) -> Result<()> {
    // Get basic package fields
    let mut package_fields = package_to_fields(package);

    // Add installation status information
    add_installation_info(&mut package_fields, package, is_installed);

    // Format and print the package fields using the shared function
    let formatted_output = crate::store::format_package_fields(&package_fields);
    print!("{}", formatted_output);

    println!(); // Empty line between packages
    Ok(())
}
