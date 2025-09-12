use std::process::Command;

fn main() {
    // Get git commit hash
    let git_hash = get_git_hash();

    // Get build date using time crate
    let build_date = get_build_date();

    let epkg_version_info = format!("version {} (build date {}, commit {})",
                                    env!("CARGO_PKG_VERSION"),
                                    build_date,
                                    git_hash);

    // Set environment variables for the build
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);
    println!("cargo:rustc-env=BUILD_DATE={}", build_date);
    println!("cargo:rustc-env=EPKG_VERSION_TAG=v{}", env!("CARGO_PKG_VERSION"));
    println!("cargo:rustc-env=EPKG_VERSION_INFO={}", epkg_version_info);
}

fn get_git_hash() -> String {
    Command::new("git")
        .args(&["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn get_build_date() -> String {
    use time::OffsetDateTime;

    OffsetDateTime::now_utc()
        .format(&time::format_description::parse("[year]-[month]-[day]").unwrap())
        .unwrap_or_else(|_| "unknown".to_string())
}
