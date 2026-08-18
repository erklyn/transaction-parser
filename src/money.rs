//! Monetary amounts.
//!
//! Money is represented with [`rust_decimal::Decimal`], a base-10 fixed-point type.
//! Binary floating point cannot represent values like `0.1` exactly, so a long run
//! of deposits and withdrawals would slowly drift away from the true balance; an
//! exact decimal keeps every intermediate balance equal to the sum of its inputs.

use rust_decimal::Decimal;

/// An amount of the single asset the engine tracks.
pub type Amount = Decimal;

/// Decimal places the engine keeps, as mandated by the specification.
pub(crate) const SCALE: u32 = 4;

/// Rounds an incoming amount to the engine's working precision.
///
/// The specification says amounts have a precision of up to four decimal places,
/// so anything longer is out of contract. Rounding (half to even, the usual
/// choice in finance because it does not bias repeated roundings upward) is more
/// forgiving than rejecting the row outright.
pub(crate) fn to_engine_scale(amount: Amount) -> Amount {
    amount.round_dp(SCALE)
}

/// Renders an amount for the output CSV, trimming trailing zeros so that round
/// values print as `2` rather than `2.0000`.
pub(crate) fn render(amount: Amount) -> String {
    to_engine_scale(amount).normalize().to_string()
}

/// Adds two amounts, or `None` if the sum cannot be represented exactly.
pub(crate) fn exact_add(left: Amount, right: Amount) -> Option<Amount> {
    exact(left.checked_add(right)?, left, right)
}

/// Subtracts `right` from `left`, or `None` if the result is not exact.
pub(crate) fn exact_sub(left: Amount, right: Amount) -> Option<Amount> {
    exact(left.checked_sub(right)?, left, right)
}

/// Rejects a result that `Decimal` rounded rather than computed exactly.
///
/// `checked_add` and `checked_sub` report `None` only when the result is too
/// large in magnitude. When it merely needs more significant digits than the
/// 96-bit mantissa holds — a balance above roughly 7.9e24 combined with a
/// four-decimal amount — they silently rescale and round instead, which in a
/// ledger means quietly creating or destroying money.
///
/// An exact sum or difference carries the scale of its wider operand, so a
/// narrower result is proof that digits were dropped — except for zero, which
/// `Decimal` may hand back with any scale. Rescaling only ever drops low-order
/// digits from a large magnitude, and a large magnitude cannot come back as
/// zero, so a zero result is exact by construction.
fn exact(result: Amount, left: Amount, right: Amount) -> Option<Amount> {
    (result.is_zero() || result.scale() >= left.scale().max(right.scale())).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Amount {
        Amount::from_str(s).unwrap()
    }

    #[test]
    fn renders_round_values_without_trailing_zeros() {
        assert_eq!(render(dec("2.0000")), "2");
        assert_eq!(render(dec("1.5000")), "1.5");
        assert_eq!(render(Amount::ZERO), "0");
    }

    #[test]
    fn renders_full_precision_when_it_is_significant() {
        assert_eq!(render(dec("1.2345")), "1.2345");
        assert_eq!(render(dec("-0.0001")), "-0.0001");
    }

    #[test]
    fn rounds_beyond_four_places_half_to_even() {
        assert_eq!(render(dec("1.00005")), "1");
        assert_eq!(render(dec("1.00015")), "1.0002");
        assert_eq!(render(dec("1.00016")), "1.0002");
    }

    #[test]
    fn decimal_arithmetic_is_exact() {
        // The same sum in f64 lands on 0.30000000000000004.
        assert_eq!(dec("0.1") + dec("0.2"), dec("0.3"));
    }

    #[test]
    fn ordinary_arithmetic_is_accepted() {
        assert_eq!(exact_add(dec("0.1"), dec("0.2")), Some(dec("0.3")));
        assert_eq!(exact_add(Amount::ZERO, dec("1.5000")), Some(dec("1.5000")));
        assert_eq!(exact_sub(dec("1.0"), dec("1.0")), Some(Amount::ZERO));
        assert_eq!(
            exact_sub(dec("0.0001"), dec("0.0002")),
            Some(dec("-0.0001"))
        );
    }

    #[test]
    fn a_zero_result_is_exact_whatever_scale_it_comes_back_with() {
        // `Decimal` answers 0.0 - 0.0 with a scale-0 zero, which is not a loss.
        assert_eq!(exact_sub(dec("100.0"), dec("100.0")), Some(Amount::ZERO));
        assert_eq!(exact_add(dec("0.0000"), Amount::ZERO), Some(Amount::ZERO));
    }

    #[test]
    fn arithmetic_that_would_silently_round_is_refused() {
        // `Decimal::checked_add` returns Some here, having dropped the 0.0001.
        let huge = dec("10000000000000000000000000");
        assert_eq!(huge.checked_add(dec("0.0001")), Some(huge));
        assert_eq!(exact_add(huge, dec("0.0001")), None);
        assert_eq!(exact_sub(huge, dec("0.0001")), None);
    }

    #[test]
    fn arithmetic_that_overflows_in_magnitude_is_refused() {
        assert_eq!(exact_add(Amount::MAX, Amount::MAX), None);
        assert_eq!(exact_sub(-Amount::MAX, Amount::MAX), None);
    }
}
