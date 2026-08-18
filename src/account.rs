//! A single client's asset account.

use crate::error::Reject;
use crate::money::{self, Amount};

/// One client's balances.
///
/// `total` is not stored: it is always `available + held`, so there is no way
/// for the three figures to disagree. Every mutation goes through a method that
/// checks the arithmetic before committing, which keeps the invariant that the
/// total is exactly representable and lets [`Account::total`] add without
/// checking.
#[derive(Debug, Clone)]
pub struct Account {
    client: u16,
    available: Amount,
    held: Amount,
    locked: bool,
}

impl Account {
    /// An account with no funds, as created the first time a client is seen.
    pub(crate) fn new(client: u16) -> Self {
        Self {
            client,
            available: Amount::ZERO,
            held: Amount::ZERO,
            locked: false,
        }
    }

    pub fn client(&self) -> u16 {
        self.client
    }

    /// Funds free for trading, staking or withdrawal.
    pub fn available(&self) -> Amount {
        self.available
    }

    /// Funds frozen pending the outcome of a dispute.
    pub fn held(&self) -> Amount {
        self.held
    }

    /// Available plus held.
    pub fn total(&self) -> Amount {
        self.available + self.held
    }

    /// Whether a chargeback has frozen this account.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Increases available funds. Used by a deposit.
    pub(crate) fn credit(&mut self, amount: Amount) -> Result<(), Reject> {
        let available = money::exact_add(self.available, amount).ok_or(Reject::Unrepresentable)?;
        self.commit(available, self.held)
    }

    /// Decreases available funds, failing if they would not cover the amount.
    /// Used by a withdrawal.
    pub(crate) fn debit(&mut self, amount: Amount) -> Result<(), Reject> {
        if self.available < amount {
            return Err(Reject::InsufficientFunds {
                client: self.client,
                available: self.available,
                requested: amount,
            });
        }
        let available = money::exact_sub(self.available, amount).ok_or(Reject::Unrepresentable)?;
        self.commit(available, self.held)
    }

    /// Freezes funds the client already has. The total does not change.
    ///
    /// Available may go negative: a client can deposit, withdraw the proceeds,
    /// and only then have the deposit disputed. The negative balance is the
    /// honest record of what they owe.
    pub(crate) fn move_available_to_held(&mut self, amount: Amount) -> Result<(), Reject> {
        let available = money::exact_sub(self.available, amount).ok_or(Reject::Unrepresentable)?;
        let held = money::exact_add(self.held, amount).ok_or(Reject::Unrepresentable)?;
        self.commit(available, held)
    }

    /// Returns frozen funds to the client. The total does not change.
    pub(crate) fn move_held_to_available(&mut self, amount: Amount) -> Result<(), Reject> {
        let available = money::exact_add(self.available, amount).ok_or(Reject::Unrepresentable)?;
        let held = money::exact_sub(self.held, amount).ok_or(Reject::Unrepresentable)?;
        self.commit(available, held)
    }

    /// Freezes funds that are not in the account, raising the total. Used when a
    /// withdrawal is disputed and a refund becomes pending.
    pub(crate) fn add_held(&mut self, amount: Amount) -> Result<(), Reject> {
        let held = money::exact_add(self.held, amount).ok_or(Reject::Unrepresentable)?;
        self.commit(self.available, held)
    }

    /// Removes frozen funds from the account entirely, lowering the total.
    pub(crate) fn remove_held(&mut self, amount: Amount) -> Result<(), Reject> {
        let held = money::exact_sub(self.held, amount).ok_or(Reject::Unrepresentable)?;
        self.commit(self.available, held)
    }

    /// Freezes the account. A chargeback is the only thing that does this, and
    /// nothing unfreezes it.
    pub(crate) fn lock(&mut self) {
        self.locked = true;
    }

    /// Applies new balances once the total is known to be exactly representable.
    fn commit(&mut self, available: Amount, held: Amount) -> Result<(), Reject> {
        money::exact_add(available, held).ok_or(Reject::Unrepresentable)?;
        self.available = available;
        self.held = held;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Amount {
        Amount::from_str(s).unwrap()
    }

    fn funded(amount: &str) -> Account {
        let mut account = Account::new(1);
        account.credit(dec(amount)).unwrap();
        account
    }

    #[test]
    fn total_always_tracks_available_plus_held() {
        let mut account = funded("10.0");
        account.move_available_to_held(dec("4.0")).unwrap();
        assert_eq!(account.available(), dec("6.0"));
        assert_eq!(account.held(), dec("4.0"));
        assert_eq!(account.total(), dec("10.0"));
    }

    #[test]
    fn debit_beyond_available_funds_is_refused_and_changes_nothing() {
        let mut account = funded("1.0");
        let outcome = account.debit(dec("1.5"));
        assert!(matches!(outcome, Err(Reject::InsufficientFunds { .. })));
        assert_eq!(account.total(), dec("1.0"));
    }

    #[test]
    fn debit_of_exactly_the_available_balance_succeeds() {
        let mut account = funded("1.0");
        account.debit(dec("1.0")).unwrap();
        assert_eq!(account.available(), Amount::ZERO);
    }

    #[test]
    fn holding_a_spent_deposit_drives_available_negative() {
        let mut account = funded("5.0");
        account.debit(dec("5.0")).unwrap();
        account.move_available_to_held(dec("5.0")).unwrap();
        assert_eq!(account.available(), dec("-5.0"));
        assert_eq!(account.held(), dec("5.0"));
        assert_eq!(account.total(), Amount::ZERO);
    }

    #[test]
    fn holding_a_pending_refund_raises_the_total() {
        let mut account = funded("1.0");
        account.add_held(dec("3.0")).unwrap();
        assert_eq!(account.available(), dec("1.0"));
        assert_eq!(account.total(), dec("4.0"));
    }

    #[test]
    fn the_two_move_methods_are_inverses() {
        let mut account = funded("10.0");
        account.move_available_to_held(dec("2.5")).unwrap();
        account.move_held_to_available(dec("2.5")).unwrap();
        assert_eq!(account.available(), dec("10.0"));
        assert_eq!(account.held(), Amount::ZERO);
    }

    #[test]
    fn the_two_held_methods_are_inverses() {
        let mut account = funded("10.0");
        account.add_held(dec("2.5")).unwrap();
        account.remove_held(dec("2.5")).unwrap();
        assert_eq!(account.total(), dec("10.0"));
        assert_eq!(account.held(), Amount::ZERO);
    }

    #[test]
    fn credit_overflow_is_refused_and_changes_nothing() {
        let mut account = Account::new(1);
        account.credit(Amount::MAX).unwrap();
        assert_eq!(account.credit(Amount::MAX), Err(Reject::Unrepresentable));
        assert_eq!(account.available(), Amount::MAX);
    }

    #[test]
    fn total_overflow_is_refused_even_when_each_field_fits() {
        let mut account = Account::new(1);
        account.credit(Amount::MAX).unwrap();
        assert_eq!(account.add_held(Amount::MAX), Err(Reject::Unrepresentable));
        assert_eq!(account.held(), Amount::ZERO);
    }

    #[test]
    fn arithmetic_that_would_lose_precision_is_refused() {
        // Decimal would answer this one by silently dropping the 0.0001.
        let mut account = funded("10000000000000000000000000");
        assert_eq!(account.credit(dec("0.0001")), Err(Reject::Unrepresentable));
        assert_eq!(account.debit(dec("0.0001")), Err(Reject::Unrepresentable));
        assert_eq!(
            account.move_available_to_held(dec("0.0001")),
            Err(Reject::Unrepresentable)
        );
        assert_eq!(account.available(), dec("10000000000000000000000000"));
        assert_eq!(account.held(), Amount::ZERO);
    }
}
