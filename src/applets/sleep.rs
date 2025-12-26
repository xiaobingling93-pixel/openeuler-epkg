use clap::{Arg, Command};
use color_eyre::Result;
use std::time::Duration;

pub struct SleepOptions {
    pub total_duration: Duration,
}

/// Parse a time interval string with optional suffix
/// Supports: NUMBER[SUFFIX] where SUFFIX can be 's', 'm', 'h', or 'd'
/// Returns duration in seconds as f64
fn parse_time_interval(s: &str) -> Result<f64> {
    if s.is_empty() {
        return Err(color_eyre::eyre::eyre!("sleep: invalid time interval '{}'", s));
    }

    // Check for suffix
    let (number_str, suffix) = if let Some(last_char) = s.chars().last() {
        match last_char {
            's' | 'm' | 'h' | 'd' => {
                let num_str = &s[..s.len() - 1];
                (num_str, Some(last_char))
            }
            _ => (s, None),
        }
    } else {
        (s, None)
    };

    // Parse the number (supports floating-point)
    let number = number_str.parse::<f64>()
        .map_err(|e| color_eyre::eyre::eyre!("sleep: invalid time interval '{}': {}", s, e))?;

    // Convert to seconds based on suffix
    let seconds = match suffix {
        Some('s') | None => number,
        Some('m') => number * 60.0,
        Some('h') => number * 3600.0,
        Some('d') => number * 86400.0,
        _ => unreachable!(), // Already checked above
    };

    Ok(seconds)
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<SleepOptions> {
    let intervals: Vec<String> = matches.get_many::<String>("number")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if intervals.is_empty() {
        return Err(color_eyre::eyre::eyre!("sleep: missing operand"));
    }

    // Parse all intervals and sum them
    let mut total_seconds = 0.0;
    for interval in &intervals {
        let seconds = parse_time_interval(interval)?;
        total_seconds += seconds;
    }

    // Convert to Duration (supports fractional seconds)
    let total_duration = Duration::from_secs_f64(total_seconds);

    Ok(SleepOptions { total_duration })
}

pub fn command() -> Command {
    Command::new("sleep")
        .about("Pause for NUMBER[SUFFIX]...")
        .long_about("Pause for NUMBER seconds, where NUMBER is an integer or floating-point.\n\
                    SUFFIX may be 's','m','h', or 'd', for seconds, minutes, hours, days.\n\
                    With multiple arguments, pause for the sum of their values.")
        .arg_required_else_help(true) // This will show help if no args are provided
        .arg(Arg::new("number")
            .required(true)
            .num_args(1..)
            .help("Number of seconds to sleep (with optional suffix: s, m, h, d)"))
}

pub fn run(options: SleepOptions) -> Result<()> {
    std::thread::sleep(options.total_duration);
    Ok(())
}

