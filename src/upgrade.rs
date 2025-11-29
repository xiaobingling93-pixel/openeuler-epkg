use crate::models::*;
use crate::install::InstallationPlan;
use color_eyre::Result;

impl PackageManager {
    pub fn upgrade_packages(&mut self, package_specs: Vec<String>) -> Result<InstallationPlan> {
        self.load_world()?;

        // Step 1: Create or load delta_world based on package_specs
        let (mut delta_world, user_request_world) = if !package_specs.is_empty() {
            let user_request_world = Self::create_delta_world_from_specs(&package_specs);
            self.apply_delta_world(&user_request_world);
            (user_request_world.clone(), Some(user_request_world))
        } else {
            (self.world.clone(), None)
        };

        // Step 2: Resolve dependencies and perform installation
        self.resolve_and_install_packages(
            &mut delta_world,
            user_request_world.as_ref(),
        )
    }

}
