//! An audit sink: ledger entries recorded then flushed. A trait and two impls
//! with different failure semantics: `VecStore` never refuses on `record`;
//! `LimitedStore` rejects with `Err(StoreError::Full)` once it holds
//! `capacity` entries. A caller of either relies on that contract.

pub mod replay;

use crate::domain::LedgerEntry;

/// A failure that stops an audit from continuing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The store is closed; no more entries may be recorded.
    Closed,
    /// The store is full; the entry was rejected, not recorded.
    Full,
}

/// An audit sink for ledger entries.
pub trait Audit {
    /// Record one entry. Fails when the store refuses it.
    fn record(&mut self, entry: LedgerEntry) -> Result<(), StoreError>;
    /// Flush the recorded entries. Returns how many were flushed.
    fn flush(&mut self) -> Result<usize, StoreError>;
    /// How many entries are currently buffered.
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An unbounded audit sink: `record` never refuses on capacity, `flush`
/// drains the buffer and reports how many entries it released.
#[derive(Debug, Clone)]
pub struct VecStore {
    buffer: Vec<LedgerEntry>,
}

impl VecStore {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }
}

impl Default for VecStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Audit for VecStore {
    fn record(&mut self, entry: LedgerEntry) -> Result<(), StoreError> {
        self.buffer.push(entry);
        Ok(())
    }

    fn flush(&mut self) -> Result<usize, StoreError> {
        let n = self.buffer.len();
        self.buffer.clear();
        Ok(n)
    }

    fn len(&self) -> usize {
        self.buffer.len()
    }
}

/// A capacity-bounded audit sink: `record` rejects with `Err(StoreError::Full)`
/// once the buffer holds `capacity` entries — the deliberate opposite of
/// `VecStore`.
#[derive(Debug, Clone)]
pub struct LimitedStore {
    capacity: usize,
    buffer: Vec<LedgerEntry>,
}

impl LimitedStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            buffer: Vec::with_capacity(capacity.min(1)),
        }
    }
}

impl Audit for LimitedStore {
    fn record(&mut self, entry: LedgerEntry) -> Result<(), StoreError> {
        if self.buffer.len() >= self.capacity {
            return Err(StoreError::Full);
        }
        self.buffer.push(entry);
        Ok(())
    }

    fn flush(&mut self) -> Result<usize, StoreError> {
        let n = self.buffer.len();
        self.buffer.clear();
        Ok(n)
    }

    fn len(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ledger::{CreditOutcome, Ledger, LedgerEntry};
    use crate::domain::money::{CreditCommand, from_str};

    fn credit(amount: &str) -> LedgerEntry {
        LedgerEntry {
            command: CreditCommand::Credit(from_str(amount).unwrap()),
            outcome: CreditOutcome::Applied {
                balance: 0,
                log_len: 0,
            },
        }
    }

    #[test]
    fn vec_store_never_refuses_and_drains_on_flush() {
        let mut store = VecStore::new();
        for _ in 0..100 {
            store.record(credit("1.00")).expect("unbounded");
        }
        assert_eq!(store.len(), 100);
        assert_eq!(store.flush().unwrap(), 100);
        assert!(store.is_empty());
    }

    #[test]
    fn limited_store_rejects_at_capacity() {
        let mut store = LimitedStore::new(2);
        store.record(credit("1.00")).unwrap();
        store.record(credit("2.00")).unwrap();
        // The third is the overflow: this is the masked capacity check.
        assert_eq!(store.record(credit("3.00")), Err(StoreError::Full));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn the_two_impls_have_different_failure_semantics() {
        // VecStore: same call that LimitedStore rejects, succeeds.
        let mut v = VecStore::new();
        v.record(credit("1.00")).unwrap();
        let mut l = LimitedStore::new(1);
        l.record(credit("1.00")).unwrap();
        assert_eq!(l.record(credit("2.00")), Err(StoreError::Full));
        assert!(v.record(credit("2.00")).is_ok());
        // the reference ledger still folds correctly through the audit path
        let mut ledger = Ledger::new();
        ledger.apply_entry(CreditCommand::Credit(from_str("5.00").unwrap()));
        assert_eq!(ledger.balance(), 500);
    }
}
