use std::fs;
use std::path::Path;
use std::process::Command;
use tar::Archive;
use clap::{Parser, Subcommand};
use zstd::stream::decode_all;
use anyhow::{Context, Result};
mod utils;
use crate::utils::is_setuid;
mod models;
use crate::models::*;

#[derive(Parser)]
#[clap(name = "epkg-store", version = "0.1.0")]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install packages into the store
    Install {
        packages: Vec<String>,
    },
    /// Garbage collect unused packages
    Gc,
}

fn main() {
    if !is_setuid() {
        eprintln!("epkg-store must be run as setuid.");
        return;
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Install { packages } => {
            for pkg_path in packages {
                install_package(&pkg_path);
            }
        }
        Commands::Gc => {
            garbage_collect();
        }
    }
}

fn install_package(pkg_path: &str) {
    let uname_output = Command::new("uname").arg("-a").output().expect("Failed to execute uname");
}

fn garbage_collect() {
    unimplemented!()
}
