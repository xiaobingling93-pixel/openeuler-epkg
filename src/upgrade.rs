use crate::plan::InstallationPlan;
use crate::models::PACKAGE_CACHE;
use crate::io::load_world;
use crate::world::{create_delta_world_from_specs, apply_delta_world};
use crate::depends::resolve_and_install_packages;
use crate::install::process_url_package_specs;
use color_eyre::Result;

pub fn upgrade_packages(package_specs: Vec<String>) -> Result<InstallationPlan> {
    load_world()?;

    // Step 1: Create or load delta_world based on package_specs
    let (mut delta_world, user_request_world) = if !package_specs.is_empty() {
        // handle local files/URLs, return all package specs ready for installation
        let processed_specs = process_url_package_specs(package_specs)?;

        let user_request_world = create_delta_world_from_specs(&processed_specs);
        apply_delta_world(&user_request_world);
        (user_request_world.clone(), Some(user_request_world))
    } else {
        (PACKAGE_CACHE.world.read().unwrap().clone(), None)
    };

    // Step 2: Resolve dependencies and perform installation
    resolve_and_install_packages(
        &mut delta_world,
        user_request_world.as_ref(),
    )
}
