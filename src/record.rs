//! Input rows, and the transactions they parse into.
//!
//! [`Record`] is the CSV's shape: whatever the file happened to contain, with a
//! type name that may not be a type and an amount that may not be there.
//! [`Transaction`] is what the ledger accepts, and the two are separated so that
//! everything which can be wrong with a *row* is settled here, before the engine
//! sees it. That is why a dispute has no amount field to ignore and a deposit
//! has no optional amount to unwrap.

use std::str::FromStr;

use serde::{Deserialize, Deserializer, de};

use crate::money::{self, Amount, SCALE};

/// Column names the input header must provide.
pub(crate) const REQUIRED_COLUMNS: [&str; 4] = ["type", "client", "tx", "amount"];

/// A transaction the engine can act on.
///
/// Every variant carries exactly the fields its kind needs, so a deposit
/// without an amount or a dispute with one cannot be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transaction {
    /// A credit to the client's account. The amount is positive and rounded to
    /// the engine's precision.
    Deposit {
        client: u16,
        tx: u32,
        amount: Amount,
    },
    /// A debit from the client's account, on the same terms.
    Withdrawal {
        client: u16,
        tx: u32,
        amount: Amount,
    },
    /// A claim that an earlier transaction was erroneous.
    Dispute { client: u16, tx: u32 },
    /// A claim withdrawn.
    Resolve { client: u16, tx: u32 },
    /// A claim upheld, reversing the transaction and freezing the account.
    Chargeback { client: u16, tx: u32 },
}

impl Transaction {
    /// The client this transaction belongs to.
    pub fn client(&self) -> u16 {
        match *self {
            Self::Deposit { client, .. }
            | Self::Withdrawal { client, .. }
            | Self::Dispute { client, .. }
            | Self::Resolve { client, .. }
            | Self::Chargeback { client, .. } => client,
        }
    }

    /// The transaction ID: this transaction's own for a deposit or withdrawal,
    /// or the one being referred to for the dispute family.
    pub fn tx(&self) -> u32 {
        match *self {
            Self::Deposit { tx, .. }
            | Self::Withdrawal { tx, .. }
            | Self::Dispute { tx, .. }
            | Self::Resolve { tx, .. }
            | Self::Chargeback { tx, .. } => tx,
        }
    }
}

/// Why a row could not be turned into a [`Transaction`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("unknown transaction type `{0}`")]
    UnknownType(String),

    #[error("a deposit or withdrawal must carry an amount")]
    MissingAmount,

    #[error("amount must be greater than zero (got {0} after rounding to {SCALE} decimal places)")]
    NonPositiveAmount(Amount),
}

/// One row of the input CSV, before any rules are applied.
///
/// `type` stays a string and `amount` stays optional, because that is what the
/// file can contain. [`Record::parse`] is where that becomes a transaction or a
/// skipped row.
#[derive(Debug, Deserialize)]
pub struct Record {
    #[serde(rename = "type")]
    pub kind: String,
    pub client: u16,
    pub tx: u32,
    /// Absent on dispute, resolve and chargeback rows, which reference a
    /// transaction by ID instead of stating an amount.
    #[serde(default, deserialize_with = "deserialize_amount")]
    pub amount: Option<Amount>,
}

impl Record {
    /// Validates the row into a transaction the engine will accept.
    pub fn parse(&self) -> Result<Transaction, ParseError> {
        let (client, tx) = (self.client, self.tx);

        // Type names are matched case-insensitively, ignoring surrounding
        // whitespace, and compared in place because this runs once per row.
        let name = self.kind.trim();
        let matches = |candidate: &str| name.eq_ignore_ascii_case(candidate);

        if matches("deposit") {
            Ok(Transaction::Deposit {
                client,
                tx,
                amount: self.amount()?,
            })
        } else if matches("withdrawal") {
            Ok(Transaction::Withdrawal {
                client,
                tx,
                amount: self.amount()?,
            })
        } else if matches("dispute") {
            Ok(Transaction::Dispute { client, tx })
        } else if matches("resolve") {
            Ok(Transaction::Resolve { client, tx })
        } else if matches("chargeback") {
            Ok(Transaction::Chargeback { client, tx })
        } else {
            Err(ParseError::UnknownType(name.to_owned()))
        }
    }

    /// The amount for a deposit or withdrawal, rounded and checked.
    ///
    /// A negative deposit would be a withdrawal that skips the sufficient-funds
    /// check, and a zero-value movement is a no-op worth reporting, so both are
    /// refused. Any amount on a dispute, resolve or chargeback is ignored: the
    /// specification says they do not state one, and the recorded transaction
    /// is what governs.
    fn amount(&self) -> Result<Amount, ParseError> {
        let amount = money::to_engine_scale(self.amount.ok_or(ParseError::MissingAmount)?);
        if amount <= Amount::ZERO {
            return Err(ParseError::NonPositiveAmount(amount));
        }
        Ok(amount)
    }
}

/// Reads the `amount` column, treating a missing or blank field as no amount.
///
/// Parsing goes through the string rather than a float so that `1.0001` is the
/// exact value written, not the nearest binary approximation of it.
fn deserialize_amount<'de, D>(deserializer: D) -> Result<Option<Amount>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) => Amount::from_str(text).map(Some).map_err(de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Amount {
        Amount::from_str(s).unwrap()
    }

    fn record(kind: &str, amount: Option<&str>) -> Record {
        Record {
            kind: kind.to_owned(),
            client: 1,
            tx: 2,
            amount: amount.map(dec),
        }
    }

    #[test]
    fn parses_the_five_transaction_types() {
        assert_eq!(
            record("deposit", Some("1.0")).parse(),
            Ok(Transaction::Deposit {
                client: 1,
                tx: 2,
                amount: dec("1.0")
            })
        );
        assert_eq!(
            record("withdrawal", Some("1.0")).parse(),
            Ok(Transaction::Withdrawal {
                client: 1,
                tx: 2,
                amount: dec("1.0")
            })
        );
        assert_eq!(
            record("dispute", None).parse(),
            Ok(Transaction::Dispute { client: 1, tx: 2 })
        );
        assert_eq!(
            record("resolve", None).parse(),
            Ok(Transaction::Resolve { client: 1, tx: 2 })
        );
        assert_eq!(
            record("chargeback", None).parse(),
            Ok(Transaction::Chargeback { client: 1, tx: 2 })
        );
    }

    #[test]
    fn type_names_are_case_and_whitespace_insensitive() {
        assert!(matches!(
            record("  DePoSiT ", Some("1.0")).parse(),
            Ok(Transaction::Deposit { .. })
        ));
        assert!(matches!(
            record("CHARGEBACK", None).parse(),
            Ok(Transaction::Chargeback { .. })
        ));
    }

    #[test]
    fn rejects_unknown_type_names() {
        assert_eq!(
            record("transfer", Some("1.0")).parse(),
            Err(ParseError::UnknownType("transfer".to_owned()))
        );
        // The reported name is trimmed but otherwise as written.
        assert_eq!(
            record("  Deposits ", Some("1.0")).parse(),
            Err(ParseError::UnknownType("Deposits".to_owned()))
        );
    }

    #[test]
    fn deposits_and_withdrawals_need_an_amount_above_zero() {
        for kind in ["deposit", "withdrawal"] {
            assert_eq!(
                record(kind, None).parse(),
                Err(ParseError::MissingAmount),
                "{kind}"
            );
            assert_eq!(
                record(kind, Some("0")).parse(),
                Err(ParseError::NonPositiveAmount(Amount::ZERO)),
                "{kind}"
            );
            assert_eq!(
                record(kind, Some("-1.0")).parse(),
                Err(ParseError::NonPositiveAmount(dec("-1.0"))),
                "{kind}"
            );
        }
    }

    #[test]
    fn amounts_are_rounded_to_four_decimal_places() {
        assert_eq!(
            record("deposit", Some("1.00005")).parse(),
            Ok(Transaction::Deposit {
                client: 1,
                tx: 2,
                amount: dec("1.0000")
            })
        );
    }

    #[test]
    fn an_amount_that_rounds_away_to_nothing_is_refused() {
        assert_eq!(
            record("deposit", Some("0.00004")).parse(),
            Err(ParseError::NonPositiveAmount(Amount::ZERO))
        );
    }

    #[test]
    fn the_dispute_family_ignores_any_amount_on_the_row() {
        assert_eq!(
            record("dispute", Some("9999.0")).parse(),
            Ok(Transaction::Dispute { client: 1, tx: 2 })
        );
    }

    #[test]
    fn client_and_tx_are_readable_for_every_variant() {
        for kind in ["deposit", "withdrawal", "dispute", "resolve", "chargeback"] {
            let transaction = record(kind, Some("1.0")).parse().expect("valid");
            assert_eq!(transaction.client(), 1, "{kind}");
            assert_eq!(transaction.tx(), 2, "{kind}");
        }
    }
}
