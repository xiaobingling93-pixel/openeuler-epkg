use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

pub struct CommOptions {
    pub file1: String,
    pub file2: String,
    pub suppress1: bool,
    pub suppress2: bool,
    pub suppress3: bool,
    pub output_delimiter: String,
    pub zero_terminated: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<CommOptions> {
    let suppress1 = matches.get_flag("suppress1");
    let suppress2 = matches.get_flag("suppress2");
    let suppress3 = matches.get_flag("suppress3");
    let output_delimiter = matches.get_one::<String>("output-delimiter")
        .cloned()
        .unwrap_or_else(|| "\t".to_string());
    let zero_terminated = matches.get_flag("zero-terminated");

    let args: Vec<String> = matches.get_many::<String>("files")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    if args.len() != 2 {
        return Err(eyre!("comm: missing operand"));
    }

    Ok(CommOptions {
        file1: args[0].clone(),
        file2: args[1].clone(),
        suppress1,
        suppress2,
        suppress3,
        output_delimiter,
        zero_terminated,
    })
}

pub fn command() -> Command {
    Command::new("comm")
        .about("Compare two sorted files line by line")
        .arg(Arg::new("suppress1")
            .short('1')
            .help("Suppress lines unique to FILE1")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("suppress2")
            .short('2')
            .help("Suppress lines unique to FILE2")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("suppress3")
            .short('3')
            .help("Suppress lines that appear in both files")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("output-delimiter")
            .long("output-delimiter")
            .help("Separate columns with STR")
            .value_name("STR"))
        .arg(Arg::new("zero-terminated")
            .short('z')
            .long("zero-terminated")
            .help("Line delimiter is NUL, not newline")
            .action(clap::ArgAction::SetTrue))
        .arg(Arg::new("files")
            .num_args(2)
            .required(true)
            .help("Two sorted files to compare (use - for stdin)"))
}

pub fn open_file_or_stdin(path: &str, applet_name: &str) -> Result<Box<dyn BufRead>> {
    if path == "-" {
        Ok(Box::new(BufReader::new(io::stdin())))
    } else {
        let file = File::open(path)
            .map_err(|e| eyre!("{}: cannot open '{}': {}", applet_name, path, e))?;
        Ok(Box::new(BufReader::new(file)))
    }
}

fn read_line(reader: &mut Box<dyn BufRead>, buf: &mut Vec<u8>, zero_terminated: bool) -> io::Result<bool> {
    buf.clear();
    let bytes_read = if zero_terminated {
        reader.read_until(b'\0', buf)?
    } else {
        reader.read_until(b'\n', buf)?
    };
    Ok(bytes_read > 0)
}

fn strip_delimiter(line: &[u8], zero_terminated: bool) -> Vec<u8> {
    if line.is_empty() {
        return line.to_vec();
    }
    let delimiter = if zero_terminated { b'\0' } else { b'\n' };
    if line.last() == Some(&delimiter) {
        line[..line.len() - 1].to_vec()
    } else {
        line.to_vec()
    }
}

fn output_line(line: &[u8], columns: usize, delimiter: &str) -> Result<()> {
    for _ in 0..columns {
        io::stdout().write_all(delimiter.as_bytes())
            .map_err(|e| eyre!("comm: error writing: {}", e))?;
    }
    io::stdout().write_all(line)
        .map_err(|e| eyre!("comm: error writing: {}", e))?;
    io::stdout().write_all(b"\n")
        .map_err(|e| eyre!("comm: error writing: {}", e))?;
    Ok(())
}

fn process_comparison(
    reader1: &mut Box<dyn BufRead>,
    reader2: &mut Box<dyn BufRead>,
    line1: &mut Vec<u8>,
    line2: &mut Vec<u8>,
    has_line1: &mut bool,
    has_line2: &mut bool,
    options: &CommOptions,
) -> Result<()> {
    let line1_content = strip_delimiter(line1, options.zero_terminated);
    let line2_content = strip_delimiter(line2, options.zero_terminated);

    match line1_content.cmp(&line2_content) {
        std::cmp::Ordering::Less => {
            // Line only in file1
            if !options.suppress1 {
                output_line(&line1_content, 0, "")?;
            }
            *has_line1 = read_line(reader1, line1, options.zero_terminated)
                .map_err(|e| eyre!("comm: error reading '{}': {}", options.file1, e))?;
        }
        std::cmp::Ordering::Greater => {
            // Line only in file2
            if !options.suppress2 {
                output_line(&line2_content, 1, &options.output_delimiter)?;
            }
            *has_line2 = read_line(reader2, line2, options.zero_terminated)
                .map_err(|e| eyre!("comm: error reading '{}': {}", options.file2, e))?;
        }
        std::cmp::Ordering::Equal => {
            // Line in both files
            if !options.suppress3 {
                output_line(&line1_content, 2, &options.output_delimiter)?;
            }
            *has_line1 = read_line(reader1, line1, options.zero_terminated)
                .map_err(|e| eyre!("comm: error reading '{}': {}", options.file1, e))?;
            *has_line2 = read_line(reader2, line2, options.zero_terminated)
                .map_err(|e| eyre!("comm: error reading '{}': {}", options.file2, e))?;
        }
    }
    Ok(())
}

pub fn run(options: CommOptions) -> Result<()> {
    let mut reader1 = open_file_or_stdin(&options.file1, "comm")?;
    let mut reader2 = open_file_or_stdin(&options.file2, "comm")?;

    let mut line1 = Vec::new();
    let mut line2 = Vec::new();

    let mut has_line1 = read_line(&mut reader1, &mut line1, options.zero_terminated)
        .map_err(|e| eyre!("comm: error reading '{}': {}", options.file1, e))?;
    let mut has_line2 = read_line(&mut reader2, &mut line2, options.zero_terminated)
        .map_err(|e| eyre!("comm: error reading '{}': {}", options.file2, e))?;

    while has_line1 || has_line2 {
        if !has_line1 {
            // Only file2 has lines
            if !options.suppress2 {
                output_line(&strip_delimiter(&line2, options.zero_terminated), 1, &options.output_delimiter)?;
            }
            has_line2 = read_line(&mut reader2, &mut line2, options.zero_terminated)
                .map_err(|e| eyre!("comm: error reading '{}': {}", options.file2, e))?;
        } else if !has_line2 {
            // Only file1 has lines
            if !options.suppress1 {
                output_line(&strip_delimiter(&line1, options.zero_terminated), 0, "")?;
            }
            has_line1 = read_line(&mut reader1, &mut line1, options.zero_terminated)
                .map_err(|e| eyre!("comm: error reading '{}': {}", options.file1, e))?;
        } else {
            // Both have lines, compare them
            process_comparison(
                &mut reader1,
                &mut reader2,
                &mut line1,
                &mut line2,
                &mut has_line1,
                &mut has_line2,
                &options,
            )?;
        }
    }

    Ok(())
}
