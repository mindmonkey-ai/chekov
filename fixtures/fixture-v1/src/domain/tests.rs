//! Direct behavioural tests for the domain. These grade the fixture's *known*
//! correct code; the held-out `hidden/` assertions grade the masked bodies the
//! candidate writes. Both are real, but they measure different things.
//!
//! Wrapped in a `#[cfg(test)]` module so the context assembler's cutter strips
//! the whole body: it names the exact-cents and near-miss answers and must
//! never reach a prompt as a visible answer key.

#[cfg(test)]
mod behaviour {
    use crate::domain::money::Cents;
    use crate::domain::{CreditCommand, CreditOutcome, Ledger, LedgerError};

    use crate::domain::money::apply;
    use crate::domain::money::from_str;

    #[test]
    fn from_str_is_exact_and_apply_is_arithmetic() {
        assert_eq!(
            apply(
                CreditCommand::Credit(from_str("10.00").unwrap()),
                Cents::default()
            ),
            Cents(1000)
        );
        assert_eq!(
            apply(
                CreditCommand::Debit(from_str("2499.95").unwrap()),
                Cents::default()
            ),
            Cents(-249995)
        );
    }

    #[test]
    fn apply_entry_advances_the_projection() {
        let mut ledger = Ledger::new();
        ledger.apply_entry(CreditCommand::Credit(from_str("2499.95").unwrap()));
        ledger.apply_entry(CreditCommand::Debit(from_str("50.00").unwrap()));
        assert_eq!(ledger.balance(), 244995);
        assert_eq!(ledger.log_len(), 2);
    }

    #[test]
    fn apply_entry_rejects_an_overdraft() {
        let mut ledger = Ledger::new();
        let err = ledger.apply_entry(CreditCommand::Debit(from_str("5.00").unwrap()));
        assert_eq!(err, CreditOutcome::Rejected(LedgerError::InsufficientFunds));
        assert_eq!(ledger.balance(), 0);
    }

    #[test]
    fn apply_entry_rejects_a_non_positive_amount() {
        let mut ledger = Ledger::new();
        let err = ledger.apply_entry(CreditCommand::Debit(from_str("-5.00").unwrap()));
        assert_eq!(err, CreditOutcome::Rejected(LedgerError::NegativeAmount));
        assert_eq!(ledger.log_len(), 0);
    }

    #[test]
    fn append_entry_is_the_near_miss_that_leaves_balance_zero() {
        // The trap: this compiles, returns Ok, and passes the obvious assertion —
        // but never advances the balance. The hidden assertion would fail it.
        let mut ledger = Ledger::new();
        ledger.append_entry(CreditCommand::Credit(from_str("100.00").unwrap()));
        assert_eq!(ledger.balance(), 0);
        assert_eq!(ledger.log_len(), 1);
    }
}
