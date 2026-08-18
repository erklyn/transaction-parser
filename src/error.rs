//! Things that can go wrong, at two levels: one row, or the whole run.

use crate::money::Amount;
use crate::record::ParseError;

/// Why a well-formed CSV row could not be applied to the ledger.
///
/// Every variant describes one row being ignored, never the run being
/// abandoned: a payments feed that stops at the first bad line is worse than
/// one that processes the other 99,999 rows and says what it dropped.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Reject {
    #[error("client {0} is locked; the account is frozen and accepts no further activity")]
    AccountLocked(u16),

    #[error("transaction {0} has already been recorded; the duplicate is ignored")]
    DuplicateTx(u32),

    #[error("transaction {0} does not exist")]
    UnknownTx(u32),

    #[error("transaction {tx} belongs to client {owner}, not client {claimed}")]
    ClientMismatch { tx: u32, owner: u16, claimed: u16 },

    #[error("client {client} has {available} available but tried to withdraw {requested}")]
    InsufficientFunds {
        client: u16,
        available: Amount,
        requested: Amount,
    },

    #[error("transaction {0} is already under dispute")]
    AlreadyDisputed(u32),

    #[error("transaction {0} is not under dispute")]
    NotDisputed(u32),

    #[error("transaction {0} has been charged back and is final")]
    ChargedBack(u32),

    #[error("the resulting balance cannot be represented exactly")]
    Unrepresentable,
}

/// Why a row was skipped, covering both malformed CSV and rejected transactions.
#[derive(Debug, thiserror::Error)]
pub enum RowError {
    /// The row could not be read or deserialized into a [`crate::record::Record`].
    #[error("malformed row: {0}")]
    Malformed(#[from] csv::Error),

    /// The row was read but does not describe a transaction.
    #[error("{0}")]
    Invalid(#[from] ParseError),

    /// The row has more fields than the header names. Deserializing it would
    /// silently keep the first few and drop the rest, so `1,000` in an amount
    /// column would post as `1`.
    #[error("expected {expected} fields to match the header, found {found}")]
    TooManyFields { expected: usize, found: usize },

    /// The row parsed cleanly but the ledger would not accept it.
    #[error("{0}")]
    Rejected(#[from] Reject),
}

/// A failure that stops the whole run, as opposed to skipping one row.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The header does not name the columns the engine needs. Every row would
    /// fail identically, so this fails once and loudly instead.
    #[error("input header must name the columns {expected}; missing {missing}")]
    Header { expected: String, missing: String },

    /// The input could not be read.
    #[error(transparent)]
    Csv(#[from] csv::Error),

    /// The output could not be written.
    #[error("writing output: {0}")]
    Output(#[from] std::io::Error),
}
