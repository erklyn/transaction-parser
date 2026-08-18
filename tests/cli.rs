//! Tests of the command line contract itself.
//!
//! The brief calls the CLI contract "very important", and automated scoring
//! runs the binary and reads stdout, so these run the real executable rather
//! than the library: argument handling, exit codes, and the rule that stdout
//! carries nothing but CSV.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The sample input shipped at the repository root, and what it must produce.
const SAMPLE: &str = "transactions.csv";
const SAMPLE_OUTPUT: &str = "client,available,held,total,locked\n\
                             1,1.5,0,1.5,false\n\
                             2,2,0,2,false\n\
                             3,0,10,10,false\n\
                             4,-25,0,-25,true\n";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn run<I: AsRef<std::ffi::OsStr>>(args: &[I]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_transaction-parser"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("the binary should be runnable")
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).expect("stdout should be UTF-8")
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).expect("stderr should be UTF-8")
}

#[test]
fn the_sample_input_produces_the_documented_output() {
    let output = run(&[SAMPLE]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), SAMPLE_OUTPUT);
}

#[test]
fn diagnostics_go_to_stderr_and_never_to_stdout() {
    let output = run(&[SAMPLE]);
    // The sample contains a withdrawal that cannot be covered.
    assert!(
        stderr(&output).contains("tried to withdraw"),
        "stderr was {:?}",
        stderr(&output)
    );
    assert!(!stdout(&output).contains("warning"));
}

#[test]
fn no_argument_fails_with_usage_and_writes_nothing_to_stdout() {
    let output = run::<&str>(&[]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("usage:"));
}

#[test]
fn more_than_one_argument_fails() {
    let output = run(&[SAMPLE, SAMPLE]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("exactly one argument"));
}

#[test]
fn a_missing_file_fails_and_names_the_path() {
    let output = run(&["no-such-file.csv"]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("no-such-file.csv"),
        "stderr was {:?}",
        stderr(&output)
    );
}

#[test]
fn a_directory_in_place_of_a_file_fails() {
    let output = run(&["tests"]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
}

#[test]
fn every_sample_case_runs_clean_through_the_binary() {
    for entry in std::fs::read_dir(repo_root().join("tests/data")).expect("tests/data") {
        let path = entry.expect("readable entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !name.ends_with(".csv") || name.ends_with(".expected.csv") {
            continue;
        }

        let output = run(&[&path]);
        assert!(output.status.success(), "{name} exited non-zero");

        let expected = std::fs::read_to_string(path.with_extension("expected.csv"))
            .unwrap_or_else(|error| panic!("reading expected output for {name}: {error}"));
        assert_eq!(stdout(&output), expected, "{name}");
    }
}
