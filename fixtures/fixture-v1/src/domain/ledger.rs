//! The event-sourced ledger. `LedgerLog` records commands; the `Ledger`
//! projection folds them into a running balance. `Ledger` exposes two entry
//! points, `append_entry` and `apply_entry`; each has a distinct contract for
//! what happens to the balance projection.

use super::money::{Cents, CreditCommand, apply};

/// Why a command was refused. Unit variants, so this is `Copy`/`Eq`/`PartialEq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerError {
    /// A `Debit` was refused because it would overdraw the account. The rule
    /// is strict: a `Debit` is refused only when its amount is *greater than*
    /// the current balance. A `Debit` of exactly the balance is allowed and
    /// settles the account to zero — the boundary belongs to the debit, not to
    /// the refusal.
    InsufficientFunds,
    /// A `Credit` or `Debit` of a non-positive amount was refused.
    NegativeAmount,
}

/// The result of folding one command into the projection. A pure value type,
/// so it is `Copy`/`Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditOutcome {
    Applied { balance: i128, log_len: usize },
    Rejected(LedgerError),
}

/// A single recorded entry on the log. Holds a `CreditOutcome`, so it inherits
/// that type's `Copy`/`Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerEntry {
    pub command: CreditCommand,
    pub outcome: CreditOutcome,
}

/// An append-only log of recorded entries. Holds a `Vec`, so it is *not*
/// `Copy`; `PartialEq`/`Eq` still hold because every element does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerLog {
    entries: Vec<LedgerEntry>,
}

impl LedgerLog {
    /// Push a command onto the log. The log records what happened; it does not
    /// itself compute a balance — that is the projection's job.
    pub fn push(&mut self, entry: LedgerEntry) {
        self.entries.push(entry);
    }

    /// The entry count on the log.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The running balance projection, folded from a `LedgerLog`. Holds a `Vec`
/// (indirectly) so it is *not* `Copy`, but `PartialEq`/`Eq` still hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    balance: i128,
    log: LedgerLog,
}

impl Ledger {
    pub fn new() -> Self {
        Self {
            balance: 0,
            log: LedgerLog::default(),
        }
    }

    /// Fold the next command into the projection and record it on the log.
    ///
    /// Advances both `balance` and `log`. Rejects a `Debit` that overdraws
    /// and any non-positive amount.
    pub fn apply_entry(&mut self, cmd: CreditCommand) -> CreditOutcome {
        match cmd {
            CreditCommand::Debit(c) if c.0 <= 0 => {
                CreditOutcome::Rejected(LedgerError::NegativeAmount)
            }
            CreditCommand::Debit(_) if !debit_allowed(self.balance, cmd) => {
                CreditOutcome::Rejected(LedgerError::InsufficientFunds)
            }
            CreditCommand::Credit(c) if c.0 <= 0 => {
                CreditOutcome::Rejected(LedgerError::NegativeAmount)
            }
            _ => {
                self.balance = apply(cmd, Cents(self.balance)).0;
                let log_len = self.log.len();
                self.log.push(LedgerEntry {
                    command: cmd,
                    outcome: CreditOutcome::Applied {
                        balance: self.balance,
                        log_len,
                    },
                });
                CreditOutcome::Applied {
                    balance: self.balance,
                    log_len: self.log.len(),
                }
            }
        }
    }

    /// Record the command on the log without folding it into the balance
    /// projection: `balance` is left unchanged.
    pub fn append_entry(&mut self, cmd: CreditCommand) -> CreditOutcome {
        let log_len = self.log.len();
        self.log.push(LedgerEntry {
            command: cmd,
            outcome: CreditOutcome::Applied {
                balance: self.balance,
                log_len,
            },
        });
        CreditOutcome::Applied {
            balance: self.balance,
            log_len: self.log.len(),
        }
    }

    /// The current projection balance.
    pub fn balance(&self) -> i128 {
        self.balance
    }

    /// The entry count on the underlying log.
    pub fn log_len(&self) -> usize {
        self.log.len()
    }
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger {
    /// Stream the recorded entries newest-first, borrowing `self` for as long
    /// as the borrow lives. Used by `store::replay_filtered`.
    pub fn replay(&self) -> std::slice::Iter<'_, LedgerEntry> {
        self.log.entries.iter()
    }
}

/// Whether a `Debit` command may be applied against `balance`.
///
/// See `LedgerError::InsufficientFunds` for the rule this enforces: a debit is
/// allowed exactly when its amount does not exceed the balance, so a debit of
/// the whole balance is allowed and settles to zero. A `Credit` is always
/// allowed here.
pub fn debit_allowed(balance: i128, cmd: CreditCommand) -> bool {
    match cmd {
        CreditCommand::Debit(c) => c.0 <= balance,
        CreditCommand::Credit(_) => true,
    }
}
