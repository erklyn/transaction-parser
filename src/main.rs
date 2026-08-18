//! Command line entry point.
//!
//! Usage: `cargo run -- transactions.csv > accounts.csv`

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use transaction_parser::run;

fn main() -> ExitCode {
    let path = match input_path() {
        Ok(path) => path,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!("usage: transaction-parser <transactions.csv>");
            return ExitCode::FAILURE;
        }
    };

    // The two failures are reported separately so that a write error on stdout
    // is never blamed on the input file.
    let input = match File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("error: {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    // Neither end is wrapped in a `BufReader`/`BufWriter`: the csv reader and
    // writer buffer internally, and their own documentation asks callers not to
    // add a second layer.
    match run(input, io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The input file is the first and only argument to the binary.
fn input_path() -> Result<PathBuf, &'static str> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("expected an input CSV file")?;
    if args.next().is_some() {
        return Err("expected exactly one argument, the input CSV file");
    }
    Ok(path.into())
}
