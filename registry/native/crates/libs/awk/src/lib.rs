//! awk implementation using the awk-rs crate (pure Rust, POSIX-compatible).
//!
//! Wraps the awk-rs library to provide a standard awk CLI interface.

use std::ffi::OsString;
use std::io::{self, BufReader};

use awk_rs::{Interpreter, Lexer, Parser};

/// Entry point for awk command.
pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0]
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    match run_awk(&str_args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("awk: {}", msg);
            2
        }
    }
}

fn run_awk(args: &[String]) -> Result<i32, String> {
    let mut field_separator = " ".to_string();
    let mut program_source: Option<String> = None;
    let mut input_files: Vec<String> = Vec::new();
    let mut variables: Vec<(String, String)> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            i += 1;
            input_files.extend(args[i..].iter().cloned());
            break;
        }

        if arg == "-F" {
            i += 1;
            if i >= args.len() {
                return Err("option -F requires an argument".to_string());
            }
            field_separator = args[i].clone();
        } else if let Some(fs) = arg.strip_prefix("-F") {
            field_separator = fs.to_string();
        } else if arg == "-v" {
            i += 1;
            if i >= args.len() {
                return Err("option -v requires an argument".to_string());
            }
            let var_assign = &args[i];
            if let Some((name, value)) = var_assign.split_once('=') {
                variables.push((name.to_string(), value.to_string()));
            } else {
                return Err(format!("invalid variable assignment: {}", var_assign));
            }
        } else if arg == "-f" {
            i += 1;
            if i >= args.len() {
                return Err("option -f requires an argument".to_string());
            }
            let content =
                std::fs::read_to_string(&args[i]).map_err(|e| format!("{}: {}", args[i], e))?;
            program_source = Some(content);
        } else if arg.starts_with('-') && arg != "-" {
            // Skip unknown options gracefully
        } else if program_source.is_none() {
            program_source = Some(arg.clone());
        } else {
            input_files.push(arg.clone());
        }

        i += 1;
    }

    let source = program_source.ok_or("no program provided")?;

    // Parse the program
    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize().map_err(|e| format!("{}", e))?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| format!("{}", e))?;

    // Create interpreter
    let mut interpreter = Interpreter::new(&program);
    interpreter.set_fs(&field_separator);

    // Set ARGC/ARGV
    let mut argv = vec!["awk".to_string()];
    argv.extend(input_files.iter().cloned());
    interpreter.set_args(argv);

    // Set variables
    for (name, value) in &variables {
        interpreter.set_variable(name, value);
    }

    // Execute
    let stdout = io::stdout();
    let mut output = stdout.lock();

    let exit_code = if input_files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let inputs = vec![BufReader::new(stdin.lock())];
        interpreter
            .run(inputs, &mut output)
            .map_err(|e| format!("{}", e))?
    } else {
        let mut exit_code = 0;
        for filename in &input_files {
            if filename == "-" {
                interpreter.set_filename("");
                let stdin = io::stdin();
                let inputs = vec![BufReader::new(stdin.lock())];
                exit_code = interpreter
                    .run(inputs, &mut output)
                    .map_err(|e| format!("{}", e))?;
            } else {
                interpreter.set_filename(filename);
                let file =
                    std::fs::File::open(filename).map_err(|e| format!("{}: {}", filename, e))?;
                let inputs = vec![BufReader::new(file)];
                exit_code = interpreter
                    .run(inputs, &mut output)
                    .map_err(|e| format!("{}", e))?;
            }
        }
        exit_code
    };

    Ok(exit_code)
}
