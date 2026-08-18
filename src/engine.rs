//! The ledger: accounts, recorded transactions, and the rules connecting them.

use std::collections::{BTreeMap, HashMap, hash_map};

use crate::account::Account;
use crate::error::Reject;
use crate::money::Amount;
use crate::record::Transaction;

/// The two transaction types that move money, and can therefore be disputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Movement {
    Deposit,
    Withdrawal,
}

/// Where a recorded transaction sits in the dispute lifecycle.
///
/// `Normal -> Disputed -> Normal` may repeat: a resolved claim can be raised
/// again. `ChargedBack` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisputeState {
    Normal,
    Disputed,
    ChargedBack,
}

/// A transaction retained so that a later dispute can reference it by ID.
#[derive(Debug, Clone, Copy)]
struct RecordedTx {
    client: u16,
    movement: Movement,
    amount: Amount,
    state: DisputeState,
}

/// Processes transactions against a set of client accounts.
///
/// The engine holds two maps: one of accounts, and one of the deposits and
/// withdrawals that a dispute might later reference. Rows are applied one at a
/// time and never buffered, so memory is bounded by the number of clients and
/// disputable transactions rather than by the size of the input.
///
/// Accounts live in a `BTreeMap` so that reporting them in client order is a
/// property of the structure rather than a step someone could forget. Clients
/// are `u16`, so the map holds at most 65,536 entries and the ordered lookup
/// costs nothing next to parsing the row it came from.
#[derive(Debug, Default)]
pub struct Engine {
    accounts: BTreeMap<u16, Account>,
    transactions: HashMap<u32, RecordedTx>,
}

impl Engine {
    /// An engine with no clients and no history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one transaction.
    ///
    /// An `Err` means it was skipped and left no trace: no balance moved, no
    /// transaction recorded, and no account created for a client the engine had
    /// not already seen. The caller can carry straight on with the next one.
    pub fn apply(&mut self, transaction: Transaction) -> Result<(), Reject> {
        let client = transaction.client();

        // A chargeback freezes the account, and a frozen account accepts nothing
        // further, not even the resolution of a dispute already in flight.
        if self.account(client).is_some_and(Account::is_locked) {
            return Err(Reject::AccountLocked(client));
        }

        match transaction {
            Transaction::Deposit { tx, amount, .. } => {
                self.record_movement(client, tx, amount, Movement::Deposit)
            }
            Transaction::Withdrawal { tx, amount, .. } => {
                self.record_movement(client, tx, amount, Movement::Withdrawal)
            }
            Transaction::Dispute { tx, .. } => self.dispute(client, tx),
            Transaction::Resolve { tx, .. } => self.resolve(client, tx),
            Transaction::Chargeback { tx, .. } => self.chargeback(client, tx),
        }
    }

    /// The account for a client, if that client has been seen.
    pub fn account(&self, client: u16) -> Option<&Account> {
        self.accounts.get(&client)
    }

    /// Every account, in ascending client order.
    pub fn accounts(&self) -> impl ExactSizeIterator<Item = &Account> {
        self.accounts.values()
    }

    /// Applies a deposit or withdrawal and records it for later dispute.
    fn record_movement(
        &mut self,
        client: u16,
        tx: u32,
        amount: Amount,
        movement: Movement,
    ) -> Result<(), Reject> {
        // Transaction IDs are globally unique, so a repeat is a replayed or
        // corrupted row. Keeping the first record stops a later row from
        // silently repointing a dispute at a different client or amount.
        let slot = match self.transactions.entry(tx) {
            hash_map::Entry::Occupied(_) => return Err(Reject::DuplicateTx(tx)),
            hash_map::Entry::Vacant(slot) => slot,
        };

        // The balance change is applied to a copy, so a refused movement leaves
        // nothing behind — not even an empty account for a client whose only
        // appearance was the row just rejected.
        let mut account = self
            .accounts
            .get(&client)
            .cloned()
            .unwrap_or_else(|| Account::new(client));

        match movement {
            Movement::Deposit => account.credit(amount)?,
            Movement::Withdrawal => account.debit(amount)?,
        }

        slot.insert(RecordedTx {
            client,
            movement,
            amount,
            state: DisputeState::Normal,
        });
        self.accounts.insert(client, account);
        Ok(())
    }

    /// Holds the funds behind a transaction while the claim is investigated.
    fn dispute(&mut self, client: u16, tx: u32) -> Result<(), Reject> {
        let recorded = self.recorded_tx(client, tx, DisputeState::Normal)?;
        let account = self.account_of(&recorded);

        match recorded.movement {
            // The client received these funds; freeze what they already have.
            Movement::Deposit => account.move_available_to_held(recorded.amount)?,
            // These funds already left the account; freeze the pending refund.
            Movement::Withdrawal => account.add_held(recorded.amount)?,
        }

        self.set_state(tx, DisputeState::Disputed);
        Ok(())
    }

    /// Drops the claim and puts the account back where it was before the dispute.
    fn resolve(&mut self, client: u16, tx: u32) -> Result<(), Reject> {
        let recorded = self.recorded_tx(client, tx, DisputeState::Disputed)?;
        let account = self.account_of(&recorded);

        match recorded.movement {
            // Give the client back the funds that were frozen.
            Movement::Deposit => account.move_held_to_available(recorded.amount)?,
            // No refund is owed after all; the withdrawal stands.
            Movement::Withdrawal => account.remove_held(recorded.amount)?,
        }

        self.set_state(tx, DisputeState::Normal);
        Ok(())
    }

    /// Reverses the transaction and freezes the account.
    fn chargeback(&mut self, client: u16, tx: u32) -> Result<(), Reject> {
        let recorded = self.recorded_tx(client, tx, DisputeState::Disputed)?;
        let account = self.account_of(&recorded);

        match recorded.movement {
            // Reversing a deposit takes the funds back out of the account.
            Movement::Deposit => account.remove_held(recorded.amount)?,
            // Reversing a withdrawal pays the pending refund into the account.
            Movement::Withdrawal => account.move_held_to_available(recorded.amount)?,
        }
        account.lock();

        self.set_state(tx, DisputeState::ChargedBack);
        Ok(())
    }

    /// Looks up the transaction a dispute, resolve or chargeback refers to, and
    /// checks that it belongs to the client on the row and is in `expected` state.
    fn recorded_tx(
        &self,
        client: u16,
        tx: u32,
        expected: DisputeState,
    ) -> Result<RecordedTx, Reject> {
        let recorded = *self.transactions.get(&tx).ok_or(Reject::UnknownTx(tx))?;

        if recorded.client != client {
            return Err(Reject::ClientMismatch {
                tx,
                owner: recorded.client,
                claimed: client,
            });
        }

        match (recorded.state, expected) {
            (state, wanted) if state == wanted => Ok(recorded),
            // Unreachable today, because a chargeback locks the owning account
            // in the same breath and `apply` refuses locked accounts before it
            // gets here. Kept deliberately: the finality of a chargeback is a
            // property of the transaction, not a side effect of the account
            // being frozen, and this is what would still enforce it if
            // unfreezing were ever added.
            (DisputeState::ChargedBack, _) => Err(Reject::ChargedBack(tx)),
            (DisputeState::Disputed, _) => Err(Reject::AlreadyDisputed(tx)),
            (DisputeState::Normal, _) => Err(Reject::NotDisputed(tx)),
        }
    }

    /// The account owning a recorded transaction.
    ///
    /// Recording a transaction always stores its account in the same call, so
    /// the account exists whenever a transaction does.
    fn account_of(&mut self, tx: &RecordedTx) -> &mut Account {
        debug_assert!(
            self.accounts.contains_key(&tx.client),
            "transaction {} refers to client {}, which has no account",
            tx.amount,
            tx.client
        );
        self.accounts
            .entry(tx.client)
            .or_insert_with(|| Account::new(tx.client))
    }

    fn set_state(&mut self, tx: u32, state: DisputeState) {
        debug_assert!(
            self.transactions.contains_key(&tx),
            "transaction {tx} was looked up moments ago and must still exist"
        );
        if let Some(recorded) = self.transactions.get_mut(&tx) {
            recorded.state = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Amount {
        Amount::from_str(s).unwrap()
    }

    fn deposit(client: u16, tx: u32, amount: &str) -> Transaction {
        Transaction::Deposit {
            client,
            tx,
            amount: dec(amount),
        }
    }

    fn withdrawal(client: u16, tx: u32, amount: &str) -> Transaction {
        Transaction::Withdrawal {
            client,
            tx,
            amount: dec(amount),
        }
    }

    fn dispute(client: u16, tx: u32) -> Transaction {
        Transaction::Dispute { client, tx }
    }

    fn resolve(client: u16, tx: u32) -> Transaction {
        Transaction::Resolve { client, tx }
    }

    fn chargeback(client: u16, tx: u32) -> Transaction {
        Transaction::Chargeback { client, tx }
    }

    /// The three transaction types that refer to an earlier transaction by ID.
    const DISPUTE_FAMILY: [fn(u16, u32) -> Transaction; 3] = [dispute, resolve, chargeback];

    /// An engine with the given transactions applied, each of which must succeed.
    fn engine_with(transactions: &[Transaction]) -> Engine {
        let mut engine = Engine::new();
        for &transaction in transactions {
            engine
                .apply(transaction)
                .unwrap_or_else(|error| panic!("setup {transaction:?} was rejected: {error}"));
        }
        engine
    }

    /// Asserts a client's four output figures.
    ///
    /// `total` is stated rather than derived from `available` and `held`, so
    /// that a change which broke the relationship between them would be caught
    /// here instead of being recomputed by the assertion.
    #[track_caller]
    fn assert_balances(
        engine: &Engine,
        client: u16,
        available: &str,
        held: &str,
        total: &str,
        locked: bool,
    ) {
        let account = engine
            .account(client)
            .unwrap_or_else(|| panic!("client {client} has no account"));
        assert_eq!(account.available(), dec(available), "available");
        assert_eq!(account.held(), dec(held), "held");
        assert_eq!(account.total(), dec(total), "total");
        assert_eq!(account.is_locked(), locked, "locked");
    }

    // --- deposits and withdrawals ---

    #[test]
    fn a_deposit_creates_the_account_and_credits_it() {
        let engine = engine_with(&[deposit(1, 1, "1.0")]);
        assert_balances(&engine, 1, "1.0", "0", "1.0", false);
    }

    #[test]
    fn a_withdrawal_debits_available_and_total() {
        let engine = engine_with(&[deposit(1, 1, "5.0"), withdrawal(1, 2, "1.5")]);
        assert_balances(&engine, 1, "3.5", "0", "3.5", false);
    }

    #[test]
    fn a_withdrawal_beyond_available_funds_leaves_the_total_unchanged() {
        let mut engine = engine_with(&[deposit(1, 1, "1.0")]);
        let outcome = engine.apply(withdrawal(1, 2, "2.0"));
        assert!(matches!(outcome, Err(Reject::InsufficientFunds { .. })));
        assert_balances(&engine, 1, "1.0", "0", "1.0", false);
    }

    #[test]
    fn a_failed_withdrawal_is_not_recorded_and_cannot_be_disputed() {
        let mut engine = engine_with(&[deposit(1, 1, "1.0")]);
        let _ = engine.apply(withdrawal(1, 2, "2.0"));
        assert_eq!(engine.apply(dispute(1, 2)), Err(Reject::UnknownTx(2)));
    }

    #[test]
    fn a_rejected_transaction_creates_no_account_for_an_unseen_client() {
        let mut engine = Engine::new();
        assert!(engine.apply(withdrawal(7, 1, "5.0")).is_err());
        assert!(engine.apply(dispute(8, 2)).is_err());
        assert_eq!(engine.accounts().count(), 0);
    }

    #[test]
    fn a_duplicate_transaction_id_creates_no_account() {
        let mut engine = engine_with(&[deposit(1, 1, "1.0")]);
        // Client 2 is new, but transaction 1 is not, so nothing should happen.
        assert_eq!(
            engine.apply(deposit(2, 1, "5.0")),
            Err(Reject::DuplicateTx(1))
        );
        assert!(engine.account(2).is_none());
        // The original transaction still belongs to client 1.
        assert_eq!(
            engine.apply(dispute(2, 1)),
            Err(Reject::ClientMismatch {
                tx: 1,
                owner: 1,
                claimed: 2,
            })
        );
    }

    #[test]
    fn clients_are_independent() {
        let engine = engine_with(&[deposit(1, 1, "1.0"), deposit(2, 2, "2.0")]);
        assert_balances(&engine, 1, "1.0", "0", "1.0", false);
        assert_balances(&engine, 2, "2.0", "0", "2.0", false);
    }

    #[test]
    fn a_repeated_transaction_id_is_ignored() {
        let mut engine = engine_with(&[deposit(1, 1, "1.0")]);
        assert_eq!(
            engine.apply(deposit(1, 1, "500.0")),
            Err(Reject::DuplicateTx(1))
        );
        assert_balances(&engine, 1, "1.0", "0", "1.0", false);
    }

    #[test]
    fn a_balance_that_could_not_be_tracked_exactly_is_refused() {
        let mut engine = engine_with(&[deposit(1, 1, "10000000000000000000000000")]);
        assert_eq!(
            engine.apply(deposit(1, 2, "0.0001")),
            Err(Reject::Unrepresentable)
        );
        assert_balances(
            &engine,
            1,
            "10000000000000000000000000",
            "0",
            "10000000000000000000000000",
            false,
        );
    }

    // --- disputing a deposit: the funds are in the account, so freeze them ---

    #[test]
    fn disputing_a_deposit_moves_funds_to_held_and_leaves_the_total() {
        let engine = engine_with(&[deposit(1, 1, "5.0"), deposit(1, 2, "2.0"), dispute(1, 1)]);
        assert_balances(&engine, 1, "2.0", "5.0", "7.0", false);
    }

    #[test]
    fn resolving_a_disputed_deposit_returns_the_funds() {
        let engine = engine_with(&[deposit(1, 1, "5.0"), dispute(1, 1), resolve(1, 1)]);
        assert_balances(&engine, 1, "5.0", "0", "5.0", false);
    }

    #[test]
    fn charging_back_a_deposit_removes_the_funds_and_locks_the_account() {
        let engine = engine_with(&[deposit(1, 1, "5.0"), dispute(1, 1), chargeback(1, 1)]);
        assert_balances(&engine, 1, "0", "0", "0", true);
    }

    #[test]
    fn the_fraud_case_leaves_the_client_owing_the_reversed_deposit() {
        // Deposit, spend the proceeds, then reverse the deposit: the negative
        // available balance is the record of what the client owes.
        let engine = engine_with(&[
            deposit(1, 1, "100.0"),
            withdrawal(1, 2, "100.0"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        assert_balances(&engine, 1, "-100.0", "0", "-100.0", true);
    }

    #[test]
    fn an_account_can_be_locked_with_funds_still_held() {
        // Two disputes in flight; charging one back freezes the account while
        // the other claim is still holding funds.
        let engine = engine_with(&[
            deposit(1, 1, "100.0"),
            withdrawal(1, 2, "60.0"),
            withdrawal(1, 3, "40.0"),
            dispute(1, 1),
            dispute(1, 3),
            chargeback(1, 3),
        ]);
        // Deposit 1 is still held (100), and the charged-back withdrawal of 40
        // was refunded into available, which the disputed deposit drove negative.
        assert_balances(&engine, 1, "-60.0", "100.0", "40.0", true);
    }

    // --- disputing a withdrawal: the funds are gone, so hold the pending refund ---

    #[test]
    fn disputing_a_withdrawal_holds_the_pending_refund() {
        let engine = engine_with(&[deposit(1, 1, "5.0"), withdrawal(1, 2, "2.0"), dispute(1, 2)]);
        assert_balances(&engine, 1, "3.0", "2.0", "5.0", false);
    }

    #[test]
    fn resolving_a_disputed_withdrawal_drops_the_refund_claim() {
        let engine = engine_with(&[
            deposit(1, 1, "5.0"),
            withdrawal(1, 2, "2.0"),
            dispute(1, 2),
            resolve(1, 2),
        ]);
        assert_balances(&engine, 1, "3.0", "0", "3.0", false);
    }

    #[test]
    fn charging_back_a_withdrawal_pays_the_refund_and_locks_the_account() {
        let engine = engine_with(&[
            deposit(1, 1, "5.0"),
            withdrawal(1, 2, "2.0"),
            dispute(1, 2),
            chargeback(1, 2),
        ]);
        assert_balances(&engine, 1, "5.0", "0", "5.0", true);
    }

    // --- dispute lifecycle rules ---

    #[test]
    fn the_dispute_family_ignores_an_unknown_transaction() {
        let mut engine = engine_with(&[deposit(1, 1, "5.0")]);
        for make in DISPUTE_FAMILY {
            assert_eq!(engine.apply(make(1, 99)), Err(Reject::UnknownTx(99)));
        }
        assert_balances(&engine, 1, "5.0", "0", "5.0", false);
    }

    #[test]
    fn the_dispute_family_ignores_a_client_with_no_account() {
        let mut engine = engine_with(&[deposit(1, 1, "5.0")]);
        for make in DISPUTE_FAMILY {
            assert_eq!(
                engine.apply(make(42, 1)),
                Err(Reject::ClientMismatch {
                    tx: 1,
                    owner: 1,
                    claimed: 42,
                })
            );
            assert_eq!(engine.apply(make(42, 99)), Err(Reject::UnknownTx(99)));
        }
        assert!(engine.account(42).is_none());
    }

    #[test]
    fn a_client_cannot_dispute_another_clients_transaction() {
        let mut engine = engine_with(&[deposit(1, 1, "5.0"), deposit(2, 2, "5.0")]);
        assert_eq!(
            engine.apply(dispute(2, 1)),
            Err(Reject::ClientMismatch {
                tx: 1,
                owner: 1,
                claimed: 2,
            })
        );
        assert_balances(&engine, 1, "5.0", "0", "5.0", false);
        assert_balances(&engine, 2, "5.0", "0", "5.0", false);
    }

    #[test]
    fn disputing_the_same_transaction_twice_is_ignored() {
        let mut engine = engine_with(&[deposit(1, 1, "5.0"), dispute(1, 1)]);
        assert_eq!(engine.apply(dispute(1, 1)), Err(Reject::AlreadyDisputed(1)));
        assert_balances(&engine, 1, "0", "5.0", "5.0", false);
    }

    #[test]
    fn resolving_or_charging_back_an_undisputed_transaction_is_ignored() {
        let mut engine = engine_with(&[deposit(1, 1, "5.0")]);
        assert_eq!(engine.apply(resolve(1, 1)), Err(Reject::NotDisputed(1)));
        assert_eq!(engine.apply(chargeback(1, 1)), Err(Reject::NotDisputed(1)));
        assert_balances(&engine, 1, "5.0", "0", "5.0", false);
    }

    #[test]
    fn a_transaction_cannot_be_charged_back_after_it_is_resolved() {
        let mut engine = engine_with(&[deposit(1, 1, "5.0"), dispute(1, 1), resolve(1, 1)]);
        assert_eq!(engine.apply(chargeback(1, 1)), Err(Reject::NotDisputed(1)));
        assert_balances(&engine, 1, "5.0", "0", "5.0", false);
    }

    #[test]
    fn a_resolved_deposit_can_be_disputed_again() {
        let engine = engine_with(&[
            deposit(1, 1, "5.0"),
            dispute(1, 1),
            resolve(1, 1),
            dispute(1, 1),
        ]);
        assert_balances(&engine, 1, "0", "5.0", "5.0", false);
    }

    #[test]
    fn a_resolved_withdrawal_can_be_disputed_again() {
        let engine = engine_with(&[
            deposit(1, 1, "5.0"),
            withdrawal(1, 2, "2.0"),
            dispute(1, 2),
            resolve(1, 2),
            dispute(1, 2),
        ]);
        assert_balances(&engine, 1, "3.0", "2.0", "5.0", false);
    }

    #[test]
    fn a_locked_account_accepts_nothing_further() {
        let mut engine = engine_with(&[
            deposit(1, 1, "5.0"),
            deposit(1, 2, "5.0"),
            dispute(1, 1),
            dispute(1, 2),
            chargeback(1, 1),
        ]);
        for refused in [
            deposit(1, 10, "1.0"),
            withdrawal(1, 11, "1.0"),
            // Including the transaction that was just charged back...
            dispute(1, 1),
            // ...and a dispute already in flight, which can no longer resolve.
            resolve(1, 2),
            chargeback(1, 2),
        ] {
            assert_eq!(engine.apply(refused), Err(Reject::AccountLocked(1)));
        }
        assert_balances(&engine, 1, "0", "5.0", "5.0", true);
    }

    #[test]
    fn one_clients_chargeback_does_not_freeze_another() {
        let mut engine = engine_with(&[
            deposit(1, 1, "5.0"),
            deposit(2, 2, "5.0"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        engine.apply(deposit(2, 3, "1.0")).unwrap();
        assert_balances(&engine, 1, "0", "0", "0", true);
        assert_balances(&engine, 2, "6.0", "0", "6.0", false);
    }

    #[test]
    fn disputes_for_several_clients_can_be_in_flight_at_once() {
        let engine = engine_with(&[
            deposit(1, 1, "10.0"),
            deposit(2, 2, "20.0"),
            deposit(3, 3, "30.0"),
            dispute(2, 2),
            dispute(1, 1),
            deposit(2, 4, "5.0"),
            resolve(1, 1),
            dispute(3, 3),
            chargeback(3, 3),
        ]);
        assert_balances(&engine, 1, "10.0", "0", "10.0", false);
        assert_balances(&engine, 2, "5.0", "20.0", "25.0", false);
        assert_balances(&engine, 3, "0", "0", "0", true);
    }

    #[test]
    fn accounts_are_listed_in_client_order() {
        let engine = engine_with(&[
            deposit(7, 1, "1.0"),
            deposit(2, 2, "1.0"),
            deposit(5, 3, "1.0"),
        ]);
        let clients: Vec<u16> = engine.accounts().map(Account::client).collect();
        assert_eq!(clients, [2, 5, 7]);
    }
}
