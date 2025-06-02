use std::path::Path;
use color_eyre::Result;

/// Unpacks an RPM package to the specified directory
/// TODO: Implement RPM unpacking functionality
pub fn unpack_package<P: AsRef<Path>>(_rpm_file: P, _store_tmp_dir: P) -> Result<()> {
    todo!("RPM package unpacking not yet implemented")
}
