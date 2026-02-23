use clap::{Arg, Command};
use color_eyre::Result;

#[derive(Debug, Clone)]
pub struct DpkgMaintscriptHelperOptions {
    pub subcommand: Option<String>,
    pub args: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<DpkgMaintscriptHelperOptions> {
    let args: Vec<String> = matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let subcommand = args.get(0).cloned();

    Ok(DpkgMaintscriptHelperOptions { subcommand, args })
}

pub fn command() -> Command {
    Command::new("dpkg-maintscript-helper")
        .about("Helper for maintainer scripts (epkg: no-op implementation)")
        .arg(
            Arg::new("args")
                .value_name("ARGS")
                .num_args(1..)
                .help("Subcommand and arguments as used in maintainer scripts"),
        )
}

fn handle_supports(args: &[String]) -> i32 {
    // Minimal support: advertise a fixed set of commands as supported.
    let supported = ["mv_conffile", "rm_conffile", "dir_to_symlink", "symlink_to_dir"];
    if let Some(name) = args.get(1) {
        if supported.contains(&name.as_str()) {
            return 0;
        }
        return 1;
    }
    1
}

pub fn run(options: DpkgMaintscriptHelperOptions) -> Result<()> {
    match options.subcommand.as_deref() {
        Some("supports") => {
            let code = handle_supports(&options.args);
            std::process::exit(code);
        }
        Some("mv_conffile") | Some("rm_conffile") | Some("dir_to_symlink") | Some("symlink_to_dir") => {
            // For now, these are implemented as safe no-ops.
            // Real dpkg logic is subtle and depends on maintainer-script context;
            // in epkg environments keeping them as successful no-ops is usually sufficient.
            std::process::exit(0);
        }
        Some(_) | None => {
            eprintln!("dpkg-maintscript-helper: unsupported or missing command (treated as no-op in epkg)");
            // Return success so maintainer scripts can continue.
            std::process::exit(0);
        }
    }
}

