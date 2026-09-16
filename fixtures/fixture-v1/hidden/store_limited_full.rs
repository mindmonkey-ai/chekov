// Held-out assertion — device 1 (cross-file first-use mask + capacity). NEVER
// materialized into prompt context; injected by the grader.
//
// `LimitedStore::record` is the single masked body in `store/`. The obvious
// wrong body is `self.buffer.push(entry)` unconditionally: it compiles and
// silently ignores capacity. The capacity check must return `Err(StoreError::Full)`
// once the buffer is at capacity.

use fixture_v1::domain::money::from_str;
use fixture_v1::domain::{CreditCommand, CreditOutcome, LedgerEntry};
use fixture_v1::store::{Audit, LimitedStore, StoreError};

fn credit(amount: &str) -> LedgerEntry {
    LedgerEntry {
        command: CreditCommand::Credit(from_str(amount).unwrap()),
        outcome: CreditOutcome::Applied { balance: 0, log_len: 0 },
    }
}

#[test]
fn limited_store_rejects_when_full() {
    let mut store = LimitedStore::new(2);
    store.record(credit("1.00")).expect("first two record");
    store.record(credit("2.00")).expect("first two record");
    // The third is the overflow: the masked capacity check must refuse it.
    assert_eq!(store.record(credit("3.00")), Err(StoreError::Full));
    assert_eq!(store.len(), 2);
}
