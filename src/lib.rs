//! A payment engine.
//!
//! Reads a CSV of deposits, withdrawals, disputes, resolutions and chargebacks,
//! applies them to per-client asset accounts, and writes the resulting balances
//! back out as CSV.
//!
//! The work is split so that the ledger rules ([`engine`]) know nothing about
//! CSV, and the CSV handling ([`csv_io`]) knows nothing about the rules. A row
//! becomes a [`Transaction`] before the engine sees it, so everything that can
//! be wrong with the *row* is settled by then. Input is streamed rather than
//! loaded, and [`csv_io::process`] takes any reader, so the same engine can sit
//! behind a file, a pipe or a socket.

pub mod account;
pub mod csv_io;
pub mod engine;
pub mod error;
pub mod money;
pub mod record;

use std::io::{Read, Write};

pub use account::Account;
pub use engine::Engine;
pub use error::{Error, Reject, RowError};
pub use money::Amount;
pub use record::{ParseError, Record, Transaction};

/// [`Amount`] is an alias for this crate's `Decimal`, re-exported so that a
/// caller can construct one without matching the dependency version by hand.
pub use rust_decimal;

/// Processes `input` and writes the resulting account table to `output`,
/// reporting every skipped row on stderr.
///
/// This is the command line's entry point. Anything that needs to route
/// diagnostics elsewhere — a server handling many streams, say, where per-row
/// warnings should not land on a shared stderr — should call
/// [`csv_io::process`] directly and pass its own handler.
pub fn run<R: Read, W: Write>(input: R, output: W) -> Result<(), Error> {
    let mut engine = Engine::new();
    csv_io::process(input, &mut engine, |row, error| {
        eprintln!("warning: row {row}: {error}");
    })?;
    csv_io::write_accounts(&engine, output)
}
