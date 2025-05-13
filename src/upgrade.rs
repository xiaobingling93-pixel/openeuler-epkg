use clap::parser::ValuesRef;
use color_eyre::Result;
use crate::models::*;

impl PackageManager {

    pub fn upgrade_packages(&self, _package_specs: ValuesRef<String>) -> Result<()> {
        if config().common.verbose {
            println!("Listing packages:");
        }
        Ok(())
    }

}
