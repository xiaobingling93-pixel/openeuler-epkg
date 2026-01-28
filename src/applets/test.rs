use clap::{Arg, Command};
use color_eyre::Result;
use std::fs;
use std::path::Path;
use std::os::unix::fs::PermissionsExt;

pub struct TestOptions {
    pub expression: Vec<String>,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<TestOptions> {
    let expression: Vec<String> = matches.get_many::<String>("expression")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Ok(TestOptions { expression })
}

pub fn command() -> Command {
    Command::new("test")
        .about("Evaluate expressions")
        .arg(Arg::new("expression")
            .num_args(0..)
            // `test`/`[` expressions include operators like `-n`, `-z`, etc.
            // Treat all remaining tokens as expression parts, even if they start with `-`.
            .allow_hyphen_values(true)
            .trailing_var_arg(true)
            .help("Expression to evaluate"))
}

fn evaluate_expression(args: &[String]) -> bool {
    fn parse_i64(s: &str) -> Option<i64> {
        s.parse::<i64>().ok()
    }

    fn eval_unary(op: &str, operand: &str) -> bool {
        match op {
            "-f" => Path::new(operand).is_file(),
            "-d" => Path::new(operand).is_dir(),
            "-e" => Path::new(operand).exists(),
            "-s" => fs::metadata(operand).map(|m| m.len() > 0).unwrap_or(false),
            "-r" => {
                match fs::metadata(operand) {
                    Ok(metadata) => {
                        #[cfg(unix)]
                        {
                            metadata.permissions().mode() & 0o400 != 0
                        }
                        #[cfg(not(unix))]
                        {
                            true
                        }
                    }
                    Err(_) => false,
                }
            }
            "-w" => {
                match fs::metadata(operand) {
                    Ok(metadata) => {
                        #[cfg(unix)]
                        {
                            metadata.permissions().mode() & 0o200 != 0
                        }
                        #[cfg(not(unix))]
                        {
                            true
                        }
                    }
                    Err(_) => false,
                }
            }
            "-x" => {
                match fs::metadata(operand) {
                    Ok(metadata) => {
                        #[cfg(unix)]
                        {
                            metadata.permissions().mode() & 0o100 != 0
                        }
                        #[cfg(not(unix))]
                        {
                            true
                        }
                    }
                    Err(_) => false,
                }
            }
            "-z" => operand.is_empty(),
            "-n" => !operand.is_empty(),
            _ => false,
        }
    }

    fn eval_binary(left: &str, op: &str, right: &str) -> bool {
        match op {
            "=" => left == right,
            "!=" => left != right,
            "-eq" => match (parse_i64(left), parse_i64(right)) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
            "-ne" => match (parse_i64(left), parse_i64(right)) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            },
            "-lt" => match (parse_i64(left), parse_i64(right)) {
                (Some(a), Some(b)) => a < b,
                _ => false,
            },
            "-le" => match (parse_i64(left), parse_i64(right)) {
                (Some(a), Some(b)) => a <= b,
                _ => false,
            },
            "-gt" => match (parse_i64(left), parse_i64(right)) {
                (Some(a), Some(b)) => a > b,
                _ => false,
            },
            "-ge" => match (parse_i64(left), parse_i64(right)) {
                (Some(a), Some(b)) => a >= b,
                _ => false,
            },
            _ => false,
        }
    }

    fn parse_primary(tokens: &[String]) -> (bool, &[String]) {
        if tokens.is_empty() {
            return (false, tokens);
        }

        // Parentheses: ( expr )
        if tokens[0] == "(" {
            let (v, rest) = parse_or(&tokens[1..]);
            if !rest.is_empty() && rest[0] == ")" {
                return (v, &rest[1..]);
            }
            return (false, rest);
        }

        // Unary operators: -n STR, -f PATH, etc.
        let op = tokens[0].as_str();
        if matches!(op, "-f" | "-d" | "-e" | "-s" | "-r" | "-w" | "-x" | "-z" | "-n") {
            if tokens.len() < 2 {
                return (false, &tokens[1..]);
            }
            return (eval_unary(op, &tokens[1]), &tokens[2..]);
        }

        // Binary operators: STR = STR, STR -eq STR, etc.
        if tokens.len() >= 3 {
            let maybe_op = tokens[1].as_str();
            if matches!(
                maybe_op,
                "=" | "!=" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"
            ) {
                return (
                    eval_binary(&tokens[0], maybe_op, &tokens[2]),
                    &tokens[3..],
                );
            }
        }

        // Single word: true if non-empty.
        (!tokens[0].is_empty(), &tokens[1..])
    }

    fn parse_not(tokens: &[String]) -> (bool, &[String]) {
        if !tokens.is_empty() && tokens[0] == "!" {
            let (v, rest) = parse_not(&tokens[1..]);
            return (!v, rest);
        }
        parse_primary(tokens)
    }

    fn parse_and(mut tokens: &[String]) -> (bool, &[String]) {
        let (mut left, mut rest) = parse_not(tokens);
        tokens = rest;
        while !tokens.is_empty() && tokens[0] == "-a" {
            let (right, next) = parse_not(&tokens[1..]);
            left = left && right;
            tokens = next;
            rest = next;
        }
        (left, rest)
    }

    fn parse_or(mut tokens: &[String]) -> (bool, &[String]) {
        let (mut left, mut rest) = parse_and(tokens);
        tokens = rest;
        while !tokens.is_empty() && tokens[0] == "-o" {
            let (right, next) = parse_and(&tokens[1..]);
            left = left || right;
            tokens = next;
            rest = next;
        }
        (left, rest)
    }

    if args.is_empty() {
        return false;
    }
    let (value, rest) = parse_or(args);
    // If we couldn't consume a syntactically valid expression, treat as false.
    if rest.is_empty() {
        value
    } else {
        false
    }
}

pub fn run(options: TestOptions) -> Result<()> {
    let result = evaluate_expression(&options.expression);

    if result {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
