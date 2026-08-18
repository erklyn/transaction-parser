//! Reading transactions in and writing account balances out.

use std::io::{Read, Write};

use crate::engine::Engine;
use crate::error::{Error, RowError};
use crate::money;
use crate::record::{REQUIRED_COLUMNS, Record};

/// Columns of the output CSV, in order.
///
/// The row written in [`write_accounts`] must follow the same order. Every
/// sample case in `tests/data` compares the whole file, header included, so a
/// change to one and not the other fails the suite.
const OUTPUT_HEADER: [&str; 5] = ["client", "available", "held", "total", "locked"];

/// Feeds every row of `input` through `engine`, reporting skipped rows to `on_skip`.
///
/// Rows are pulled one at a time into a single reused buffer, so a 10 GB file
/// and a 10 KB file cost the same in read-side memory. The `Read` bound means
/// the caller supplies the source: a file here, but equally a socket if this
/// engine were embedded in a server.
///
/// Two things are fatal: an unreadable header, and an I/O failure on the
/// underlying reader. Everything else — a row that cannot be parsed, or that
/// the ledger refuses — is handed to `on_skip` with its 1-based data row number
/// and processing continues.
pub fn process<R, F>(input: R, engine: &mut Engine, mut on_skip: F) -> Result<(), Error>
where
    R: Read,
    F: FnMut(u64, RowError),
{
    let mut reader = csv::ReaderBuilder::new()
        // The specification requires tolerating whitespace around every field.
        .trim(csv::Trim::All)
        // Dispute rows carry no amount and may simply stop after the tx column.
        // Rows *longer* than the header are rejected below rather than trimmed.
        .flexible(true)
        .from_reader(input);

    let headers = normalized_headers(&mut reader)?;
    let mut row = csv::StringRecord::new();
    let mut number = 0;

    loop {
        match reader.read_record(&mut row) {
            Ok(true) => number += 1,
            Ok(false) => return Ok(()),
            Err(error) if error.is_io_error() => return Err(error.into()),
            Err(error) => {
                number += 1;
                on_skip(number, error.into());
                continue;
            }
        }

        // Serde would take the first few fields by header position and drop the
        // rest, so a stray thousands separator in `1,000` would post as `1`.
        if row.len() > headers.len() {
            on_skip(
                number,
                RowError::TooManyFields {
                    expected: headers.len(),
                    found: row.len(),
                },
            );
            continue;
        }

        match row.deserialize::<Record>(Some(&headers)) {
            Ok(record) => match record.parse() {
                Ok(transaction) => {
                    if let Err(reject) = engine.apply(transaction) {
                        on_skip(number, reject.into());
                    }
                }
                Err(invalid) => on_skip(number, invalid.into()),
            },
            Err(error) => on_skip(number, error.into()),
        }
    }
}

/// Writes the account table as CSV.
pub fn write_accounts<W: Write>(engine: &Engine, output: W) -> Result<(), Error> {
    let mut writer = csv::Writer::from_writer(output);
    writer.write_record(OUTPUT_HEADER)?;

    for account in engine.accounts() {
        writer.write_record([
            account.client().to_string(),
            money::render(account.available()),
            money::render(account.held()),
            money::render(account.total()),
            account.is_locked().to_string(),
        ])?;
    }

    writer.flush()?;
    Ok(())
}

/// Reads the header, lower-cases it, and checks it names the columns we need.
///
/// Column names are matched case-insensitively for the same reason transaction
/// types are. Without this check a header of `Type,Client,Tx,Amount` would
/// produce an identical "missing field" warning for every row in the file and
/// still exit successfully with an empty table, which is the worst of both
/// worlds: noisy and silent at once.
fn normalized_headers<R: Read>(reader: &mut csv::Reader<R>) -> Result<csv::StringRecord, Error> {
    let headers: csv::StringRecord = reader
        .headers()?
        .iter()
        .map(|column| column.trim().to_ascii_lowercase())
        .collect();

    // An input with no bytes at all has no header to be wrong about; it simply
    // has no rows, and reports an empty account table.
    if headers.is_empty() {
        return Ok(headers);
    }

    let missing: Vec<&str> = REQUIRED_COLUMNS
        .iter()
        .copied()
        .filter(|required| !headers.iter().any(|column| column == *required))
        .collect();

    if missing.is_empty() {
        Ok(headers)
    } else {
        Err(Error::Header {
            expected: REQUIRED_COLUMNS.join(", "),
            missing: missing.join(", "),
        })
    }
}
