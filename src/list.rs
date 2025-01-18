use anyhow::Result;
use crate::models::*;

impl PackageManager {

    pub fn list_packages(&self, glob_pattern: &str) -> Result<()> {
        if self.options.verbose {
            println!("Listing packages:");
        }
        Ok(())
    }

}
