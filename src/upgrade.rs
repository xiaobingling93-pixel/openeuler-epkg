use clap::parser::ValuesRef;
use anyhow::Result;
use crate::models::*;

impl PackageManager {

    pub fn upgrade_packages(&self, package_specs: ValuesRef<String>) -> Result<()> {
        if self.options.verbose {
            println!("Listing packages:");
        }
        Ok(())
    }

}
