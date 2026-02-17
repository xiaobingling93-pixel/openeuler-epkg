use clap::{Arg, Command};
use color_eyre::Result;

pub struct NprocOptions {
    pub all: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<NprocOptions> {
    let all = matches.get_flag("all");

    Ok(NprocOptions { all })
}

pub fn command() -> Command {
    Command::new("nproc")
        .about("Print the number of processing units available")
        .arg(Arg::new("all")
            .short('a')
            .long("all")
            .action(clap::ArgAction::SetTrue)
            .help("Print the number of installed processors"))
}

pub fn run(options: NprocOptions) -> Result<()> {
    let num = if options.all {
        num_cpus::get()
    } else {
        // Get the number of processors available to the current process
        // This is typically the same as num_cpus::get() unless CPU affinity is set
        num_cpus::get()
    };

    println!("{}", num);
    Ok(())
}
