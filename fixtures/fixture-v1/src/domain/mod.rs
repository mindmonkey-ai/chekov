//! Domain entities for the fixture ledger.
//!
//! The invariant that matters lives here, stated once in a comment:
//! balances are `i128` cents and are *never* constructed from an `f64`.
//! A candidate that builds a `Cents` constructor which rounds through a float
//! compiles, passes the obvious behavioural assertion, and still produces a
//! wrong answer on `2499.95` (`249994` via float, `249995` via str) — a
//! failure hidden behind idiomatic-looking code. That is the invariant trap
//! (3 masked bodies). `domain/mod.rs` documents the rule the masked bodies
//! must honour.
//!
//! Tiers 1–2 of the grader are line-level and deliberately do NOT apply to
//! these entity constructors; the invariant is enforced at the compile/test
//! tiers (6–7) against the held-out assertions in `hidden/`.

pub mod money;
pub mod ledger;

pub use money::{Cents, CreditCommand};
pub use ledger::{CreditOutcome, LedgerEntry, LedgerError, Ledger, LedgerLog};

#[cfg(test)]
mod tests;
