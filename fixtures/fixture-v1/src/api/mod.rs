//! The command dispatcher over a ledger. `Ledger` exposes two entry APIs,
//! `append_entry` and `apply_entry`; the dispatcher's job is to keep the
//! running balance projection current as commands arrive.

use crate::domain::ledger::{CreditOutcome, Ledger};
use crate::domain::money::CreditCommand;

/// A credit dispatcher over a ledger.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    ledger: Ledger,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            ledger: Ledger::new(),
        }
    }

    /// The current projection balance.
    pub fn balance(&self) -> i128 {
        self.ledger.balance()
    }

    /// Dispatch a credit command to the ledger.
    ///
    /// Must fold the command into the running projection and return the
    /// exact `CreditOutcome` the ledger produced.
    pub fn handle_credit(&mut self, cmd: CreditCommand) -> CreditOutcome {
        let outcome = self.ledger.apply_entry(cmd);
        if let CreditOutcome::Applied { balance, .. } = outcome {
            assert_eq!(self.balance(), balance);
        }
        outcome
    }

    /// Dispatch a debit command through the same path as a credit.
    pub fn handle_debit(&mut self, cmd: CreditCommand) -> CreditOutcome {
        self.handle_credit(cmd)
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ledger::{CreditOutcome, LedgerError};
    use crate::domain::money::{apply, Cents};
    use crate::domain::money::from_str;

    #[test]
    fn apply_entry_advances_balance() {
        let mut d = Dispatcher::new();
        d.handle_credit(CreditCommand::Credit(from_str("2499.95").unwrap()));
        assert_eq!(d.balance(), 249995);
        d.handle_debit(CreditCommand::Debit(from_str("50.00").unwrap()));
        assert_eq!(d.balance(), 244995);
    }

    #[test]
    fn debit_overdraft_is_rejected() {
        let mut d = Dispatcher::new();
        let err = d.handle_debit(CreditCommand::Debit(from_str("5.00").unwrap()));
        assert_eq!(err, CreditOutcome::Rejected(LedgerError::InsufficientFunds));
    }

    #[test]
    fn apply_and_append_are_the_near_miss() {
        // prove the near-miss: append leaves the balance at 0, apply does not
        let mut ledger = Ledger::new();
        ledger.append_entry(CreditCommand::Credit(from_str("100.00").unwrap()));
        assert_eq!(ledger.balance(), 0);
        ledger.apply_entry(CreditCommand::Credit(from_str("100.00").unwrap()));
        assert_eq!(ledger.balance(), 10000);
        // pure-arithmetic sanity: apply over Cents is just arithmetic
        assert_eq!(apply(CreditCommand::Credit(Cents(250)), Cents(0)), Cents(250));
    }
}
