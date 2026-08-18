//! End-to-end tests over the sample data in `tests/data`.
//!
//! Each case is a pair of files: `<name>.csv` is fed to the engine and the
//! output must match `<name>.expected.csv` byte for byte. Adding a case means
//! adding a pair of files and one name to `SAMPLE_CASES`.

use std::fs;
use std::path::{Path, PathBuf};

use transaction_parser::{Engine, Error, Reject, RowError, csv_io, run};

/// Every sample case that must be present and passing.
///
/// Naming them here rather than trusting a directory scan means deleting a
/// fixture fails the suite instead of quietly shrinking it.
const SAMPLE_CASES: [&str; 6] = [
    "spec_example",
    "dispute_lifecycle",
    "fraud_reversal",
    "skipped_rows",
    "precision_and_spacing",
    "held_across_clients",
];

/// The variant name at the head of a `Debug` rendering, without its payload.
fn variant_name(debug: &str) -> String {
    debug
        .split(['(', ' '])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// Runs one sample input through the engine and returns the output CSV.
fn output_for(input: &str) -> String {
    let mut output = Vec::new();
    run(input.as_bytes(), &mut output).expect("processing should not fail");
    String::from_utf8(output).expect("output should be UTF-8")
}

/// Collects the rows a run skipped, as `(row number, error)`.
fn skipped_rows(input: &str) -> Vec<(u64, RowError)> {
    let mut engine = Engine::new();
    let mut skipped = Vec::new();
    csv_io::process(input.as_bytes(), &mut engine, |row, error| {
        skipped.push((row, error));
    })
    .expect("processing should not fail");
    skipped
}

#[test]
fn sample_data_produces_the_expected_accounts() {
    for case in SAMPLE_CASES {
        let input = fs::read_to_string(data_dir().join(format!("{case}.csv")))
            .unwrap_or_else(|error| panic!("reading {case}.csv: {error}"));
        let expected = fs::read_to_string(data_dir().join(format!("{case}.expected.csv")))
            .unwrap_or_else(|error| panic!("reading {case}.expected.csv: {error}"));

        assert_eq!(output_for(&input), expected, "sample case {case}");
    }
}

#[test]
fn every_sample_file_on_disk_is_listed_as_a_case() {
    let mut found: Vec<String> = fs::read_dir(data_dir())
        .expect("tests/data should exist")
        .filter_map(|entry| {
            let path = entry.expect("readable directory entry").path();
            let name = path.file_name()?.to_str()?.to_owned();
            let stem = name.strip_suffix(".csv")?;
            (!stem.ends_with(".expected")).then(|| stem.to_owned())
        })
        .collect();
    found.sort();

    let mut expected: Vec<String> = SAMPLE_CASES.iter().map(|&s| s.to_owned()).collect();
    expected.sort();

    assert_eq!(found, expected, "tests/data and SAMPLE_CASES disagree");
}

#[test]
fn an_empty_file_produces_a_header_and_nothing_else() {
    assert_eq!(output_for(""), "client,available,held,total,locked\n");
    assert_eq!(
        output_for("type,client,tx,amount\n"),
        "client,available,held,total,locked\n"
    );
}

#[test]
fn a_file_whose_every_row_is_rejected_produces_no_accounts() {
    let input = "type,client,tx,amount\n\
                 withdrawal,1,1,5.0\n\
                 dispute,2,404,\n\
                 transfer,3,5,1.0\n";
    assert_eq!(output_for(input), "client,available,held,total,locked\n");
}

#[test]
fn dispute_rows_may_omit_the_amount_column_entirely() {
    // Three fields rather than four, with no trailing comma.
    let output = output_for("type,client,tx,amount\ndeposit,1,1,5.0\ndispute,1,1\n");
    assert_eq!(
        output,
        "client,available,held,total,locked\n1,0,5,5,false\n"
    );
}

#[test]
fn columns_may_appear_in_any_order_and_any_case() {
    // The reader maps fields by header name, not by position.
    let output = output_for("Amount,TX,Client,TYPE\n1.0,1,1,deposit\n");
    assert_eq!(
        output,
        "client,available,held,total,locked\n1,1,0,1,false\n"
    );
}

#[test]
fn a_header_missing_the_required_columns_stops_the_run() {
    let mut engine = Engine::new();
    let outcome = csv_io::process("a,b,c,d\n1,2,3,4\n".as_bytes(), &mut engine, |_, _| {});
    let Err(Error::Header { missing, .. }) = outcome else {
        panic!("expected a header error, got {outcome:?}");
    };
    assert_eq!(missing, "type, client, tx, amount");
}

#[test]
fn a_row_longer_than_the_header_is_skipped_rather_than_truncated() {
    // Without this check the stray thousands separator would post as `1`.
    let input = "type,client,tx,amount\n\
                 deposit,1,1,1000\n\
                 deposit,2,2,1,000\n";
    assert_eq!(
        output_for(input),
        "client,available,held,total,locked\n1,1000,0,1000,false\n"
    );
    assert!(matches!(
        skipped_rows(input).as_slice(),
        [(
            2,
            RowError::TooManyFields {
                expected: 4,
                found: 5
            }
        )]
    ));
}

#[test]
fn skipped_rows_are_reported_with_their_row_number() {
    let input = "type,client,tx,amount\n\
                 deposit,1,1,1.0\n\
                 withdrawal,1,2,99.0\n\
                 dispute,1,404,\n";

    let skipped = skipped_rows(input);
    assert!(
        matches!(
            skipped.as_slice(),
            [
                (
                    2,
                    RowError::Rejected(Reject::InsufficientFunds { client: 1, .. })
                ),
                (3, RowError::Rejected(Reject::UnknownTx(404))),
            ]
        ),
        "{skipped:?}"
    );
}

#[test]
fn row_numbers_ignore_blank_lines_and_line_endings() {
    // The bad row is the second *data* row in all three files, though it sits
    // on a different physical line in each.
    for input in [
        "type,client,tx,amount\ndeposit,1,1,1.0\ndeposit,1,2,-5\n",
        "type,client,tx,amount\r\ndeposit,1,1,1.0\r\ndeposit,1,2,-5\r\n",
        "type,client,tx,amount\ndeposit,1,1,1.0\n\n\n\ndeposit,1,2,-5\n",
    ] {
        let skipped = skipped_rows(input);
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].0, 2, "{skipped:?}");
    }
}

#[test]
fn the_skipped_rows_sample_exercises_every_rejection_reason() {
    let input = fs::read_to_string(data_dir().join("skipped_rows.csv")).expect("readable sample");
    let reasons: Vec<String> = skipped_rows(&input)
        .into_iter()
        .map(|(_, error)| match error {
            RowError::Malformed(_) => "malformed".to_owned(),
            RowError::TooManyFields { .. } => "too many fields".to_owned(),
            // The variant name alone, so the assertion pins which rule fired
            // without restating every field it carries. Parse failures and
            // ledger rejections are both named, since the point is to show the
            // sample reaches every one of them.
            RowError::Invalid(invalid) => variant_name(&format!("{invalid:?}")),
            RowError::Rejected(reject) => variant_name(&format!("{reject:?}")),
        })
        .collect();

    assert_eq!(
        reasons,
        [
            "UnknownType",
            "DuplicateTx",
            "MissingAmount",
            "NonPositiveAmount",
            "NonPositiveAmount",
            "InsufficientFunds",
            "UnknownTx",
            "ClientMismatch",
            "NotDisputed",
            "NotDisputed",
            "malformed",
            "too many fields",
            "InsufficientFunds",
        ]
    );
}

#[test]
fn a_malformed_row_does_not_stop_the_rows_after_it() {
    let input = "type,client,tx,amount\n\
                 deposit,1,1,1.0\n\
                 deposit,not-a-client,2,1.0\n\
                 deposit,1,3,2.0\n";
    assert_eq!(
        output_for(input),
        "client,available,held,total,locked\n1,3,0,3,false\n"
    );
}
