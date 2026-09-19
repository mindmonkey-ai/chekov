//! Domain entities for the fixture ledger.
//!
//! The invariant that matters: balances are `i128` cents and are never
//! constructed from an `f64`.

pub mod money;
pub mod ledger;

pub use money::{Cents, CreditCommand};
pub use ledger::{CreditOutcome, LedgerEntry, LedgerError, Ledger, LedgerLog};

#[cfg(test)]
mod tests;
