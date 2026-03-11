use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::io::{self, Write, BufRead, BufReader};
use std::fs;
use mlua::Lua;
use crate::shebang::strip_shebang;

pub struct RpmluaOptions {
    pub execute: Option<String>,
    pub interactive: bool,
    pub opts: Option<String>,
    pub script_file: Option<String>,
    pub script_args: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RpmluaOptions> {
    let execute = matches.get_one::<String>("execute").cloned();
    let interactive = matches.get_flag("interactive");
    let opts = matches.get_one::<String>("opts").cloned();
    let script_file = matches.get_one::<String>("script_file").cloned();
    let script_args: Vec<String> = matches.get_many::<String>("script_args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(RpmluaOptions {
        execute,
        interactive,
        opts,
        script_file,
        script_args,
    })
}

pub fn command() -> Command {
    Command::new("rpmlua")
        .about("RPM Lua interpreter")
        .arg(Arg::new("execute")
            .short('e')
            .long("execute")
            .value_name("STATEMENT")
            .help("Execute a Lua statement"))
        .arg(Arg::new("interactive")
            .short('i')
            .long("interactive")
            .action(clap::ArgAction::SetTrue)
            .help("Run an interactive session"))
        .arg(Arg::new("opts")
            .long("opts")
            .value_name("OPTSTRING")
            .help("Perform getopt(3) option processing on the passed arguments"))
        .arg(Arg::new("script_file")
            .help("Lua script file to execute")
            .index(1))
        .arg(Arg::new("script_args")
            .help("Arguments to pass to the script")
            .num_args(0..)
            .index(2))
}

pub fn run(options: RpmluaOptions) -> Result<()> {
    // Get the global cached Lua state with extensions
    let lua = crate::lua::get_cached_lua_state();

    // Execute statement if provided
    if let Some(ref statement) = options.execute {
        run_script(&lua, statement, "<execute>", &options.opts, &options.script_args)?;
    }

    // Execute script file if provided
    if let Some(ref script_file) = options.script_file {
        let script_content = fs::read_to_string(script_file)
            .map_err(|e| eyre!("rpmlua: failed to read script file '{}': {}", script_file, e))?;
        run_script(&lua, &script_content, script_file, &options.opts, &options.script_args)?;
    }

    // Run interactive mode if requested or if no script/statement provided
    if options.interactive || (options.execute.is_none() && options.script_file.is_none()) {
        run_interactive(&lua)?;
    }

    Ok(())
}

/// Run a Lua script with option processing and argument setup
fn run_script(
    lua: &Lua,
    script: &str,
    name: &str,
    opts: &Option<String>,
    args: &[String],
) -> Result<()> {
    // Create opt table (always create, even if opts is None)
    let opt_table = lua.create_table()?;
    let arg_start_index = if let Some(ref optstring) = opts {
        // Process options and get the index where non-option arguments start
        process_options(optstring, args, &opt_table)?
    } else {
        0
    };

    // Create arg table with script arguments
    // Note: In RPM Lua, arg[1] is the script name, arg[2+] are the non-option arguments
    let arg_table = lua.create_table()?;
    arg_table.set(1, name)?;
    // Only include arguments after processed options
    for (i, arg) in args.iter().skip(arg_start_index).enumerate() {
        arg_table.set(i + 2, arg.as_str())?;
    }

    // Wrap script with local opt, arg = ...; prefix (as in reference implementation)
    let stripped_script = strip_shebang(script);
    // Defensive fallback: if shebang still present, strip first line manually
    let final_script = if stripped_script.trim_start().starts_with("#!") {
        if let Some(pos) = stripped_script.find('\n') {
            &stripped_script[pos+1..]
        } else {
            ""
        }
    } else {
        stripped_script
    };
    let wrapped_script = format!("local opt, arg = ...; {}", final_script);

    // Load the script as a function
    let func: mlua::Function = lua.load(&wrapped_script)
        .set_name(name)
        .into_function()?;

    // Call the function with opt and arg tables as arguments
    func.call::<()>((opt_table, arg_table))
        .map_err(|e| eyre!("rpmlua: script execution failed: {}", e))?;

    Ok(())
}

/// Parse optstring into a map of option characters to whether they take values
/// Returns a HashMap where the key is the option character and the value indicates if it takes a value
/// Example: "ab:c" -> {'a': false, 'b': true, 'c': false}
fn parse_optstring(optstring: &str) -> std::collections::HashMap<char, bool> {
    use std::collections::HashMap;

    let mut option_specs: HashMap<char, bool> = HashMap::new();
    let mut chars = optstring.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == ':' {
            continue; // Skip colons (used for error handling in getopt)
        }
        let takes_value = chars.peek() == Some(&':');
        option_specs.insert(ch, takes_value);
        if takes_value {
            chars.next(); // Skip the ':'
        }
    }

    option_specs
}

/// Process arguments using getopt-style parsing
/// Populates the opt_table with parsed options and returns the index where non-option arguments start
fn process_option_args(
    option_specs: &std::collections::HashMap<char, bool>,
    args: &[String],
    opt_table: &mlua::Table,
) -> Result<usize> {
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            // End of options marker
            i += 1;
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            let option_char = arg.chars().nth(1).unwrap();
            if let Some(&takes_value) = option_specs.get(&option_char) {
                if takes_value {
                    // Option takes a value
                    if arg.len() > 2 {
                        // Value is in the same argument (e.g., -b5)
                        let value = &arg[2..];
                        opt_table.set(option_char.to_string(), value)?;
                    } else if i + 1 < args.len() {
                        // Value is in the next argument
                        i += 1;
                        opt_table.set(option_char.to_string(), args[i].as_str())?;
                    } else {
                        // Missing value
                        return Err(eyre!("rpmlua: option '{}' requires a value", option_char));
                    }
                } else {
                    // Option doesn't take a value
                    opt_table.set(option_char.to_string(), "")?;
                }
            } else {
                // Unknown option
                return Err(eyre!("rpmlua: unknown option '{}'", option_char));
            }
        } else {
            // Not an option, stop processing
            break;
        }
        i += 1;
    }

    Ok(i)
}

/// Process options using getopt-style parsing
/// Returns the index where non-option arguments start
fn process_options(
    optstring: &str,
    args: &[String],
    opt_table: &mlua::Table,
) -> Result<usize> {
    let option_specs = parse_optstring(optstring);
    process_option_args(&option_specs, args, opt_table)
}

/// Run interactive Lua session

fn handle_multi_line_input(
    lua: &Lua,
    reader: &mut BufReader<io::StdinLock>,
    mut full_code: String,
) -> Result<()> {
    loop {
        print!(">> ");
        io::stdout().flush()?;

        let mut cont_line = String::new();
        let cont_bytes = reader.read_line(&mut cont_line)?;
        if cont_bytes == 0 {
            break;
        }

        full_code.push('\n');
        full_code.push_str(cont_line.trim());

        match execute_interactive_line(lua, &full_code) {
            Ok(_) => break,
            Err(e2) => {
                let e2_str = e2.to_string();
                if !e2_str.contains("near `<eof>'") && !e2_str.contains("unexpected <eof>") {
                    eprintln!("{}", e2);
                    break;
                }
            }
        }
    }
    Ok(())
}

enum Control {
    Continue,
    Break,
    Skip,
}

fn run_interactive(lua: &Lua) -> Result<()> {
    // Check if we're in a TTY
    let is_tty = unsafe {
        libc::isatty(libc::STDOUT_FILENO) != 0 && libc::isatty(libc::STDIN_FILENO) != 0
    };
    if !is_tty {
        return Err(eyre!("rpmlua: interactive mode requires a TTY"));
    }

    // Get Lua version from the Lua state
    let lua_version = lua.globals().get::<String>("_VERSION")
        .unwrap_or_else(|_| "Lua 5.4".to_string());

    println!("\nRPM Interactive {} Interpreter", lua_version);

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());

    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            // EOF (Ctrl+D)
            println!();
            break;
        }

        match process_line(lua, &mut reader, line)? {
            Control::Continue => (),
            Control::Break => break,
            Control::Skip => continue,
        }
    }

    println!();
    Ok(())
}

fn execute_interactive_code(
    lua: &Lua,
    reader: &mut BufReader<io::StdinLock>,
    code: String,
) -> Result<()> {
    match execute_interactive_line(lua, &code) {
        Ok(_) => Ok(()),
        Err(e) => {
            // Check if it's a syntax error that might need more input
            let error_str = e.to_string();
            if error_str.contains("near `<eof>'") || error_str.contains("unexpected <eof>") {
                handle_multi_line_input(lua, reader, code)?;
                Ok(())
            } else {
                eprintln!("{}", e);
                Ok(())
            }
        }
    }
}

fn process_line(
    lua: &Lua,
    reader: &mut BufReader<io::StdinLock>,
    line: String,
) -> Result<Control> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(Control::Skip);
    }
    if line == "exit" || line == "quit" {
        return Ok(Control::Break);
    }
    let code = if line.starts_with('=') {
        format!("print({})", &line[1..])
    } else {
        line.to_string()
    };
    execute_interactive_code(lua, reader, code)?;
    Ok(Control::Continue)
}

/// Execute a single line of Lua code in interactive mode
fn execute_interactive_line(lua: &Lua, code: &str) -> Result<()> {
    // Try to load and execute
    let chunk = lua.load(code).set_name("<interactive>");

    match chunk.exec() {
        Ok(_) => Ok(()),
        Err(e) => {
            let err_str = e.to_string();
            // Check if it's an "unexpected symbol" error for what might be a bare expression
            if err_str.contains("unexpected symbol") || err_str.contains("syntax error near") {
                // Try wrapping in print() as it might be a bare expression
                let wrapped = format!("print({})", code);
                let chunk2 = lua.load(&wrapped).set_name("<interactive>");
                match chunk2.exec() {
                    Ok(_) => Ok(()),
                    Err(e2) => Err(eyre!("{}", e2)),
                }
            } else {
                Err(eyre!("{}", e))
            }
        },
    }
}

