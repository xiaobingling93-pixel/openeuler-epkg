use clap::parser::ValuesRef;
use anyhow::Result;
use crate::models::*;

impl PackageManager {

    pub fn remove_packages(&self, package_specs: ValuesRef<String>) -> Result<()> {
        if self.options.verbose {
            println!("Removing packages: {:?}", package_specs);
        }
        Ok(())
    }

}
