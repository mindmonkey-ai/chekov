// Held-out assertion — device 4 (generic + lifetime knot). NEVER materialized
// into prompt context; injected by the grader.
//
// `replay_filtered` is the single masked body in `store/replay.rs`. The
// signature type-checks exactly one way: the returned iterator must borrow
// `entries` for `'a`, and the filter closure must capture `filter` for that
// SAME `'a`. A near-miss that lets the closure capture `filter` for a shorter
// lifetime than the returned iterator fails to compile (the closure outlives
// the value it borrows) — so this task is graded by the compile gate (tier 6),
// not the test gate.

use fixture_v1::domain::money::{CreditCommand, from_str};
use fixture_v1::domain::{CreditOutcome, LedgerEntry};
use fixture_v1::store::replay::{count_filtered, Filter};

fn credit(amount: &str) -> LedgerEntry {
    let c = from_str(amount).unwrap();
    LedgerEntry {
        command: CreditCommand::Credit(c),
        outcome: CreditOutcome::Applied { balance: c.0, log_len: 0 },
    }
}

#[test]
fn replay_filtered_borrows_entries_and_filter_for_the_same_a() {
    let entries = [credit("1.00"), credit("2.00"), credit("0.50")];
    let filter = Filter::min_credits(100);
    // Only 1.00 and 2.00 survive the filter (>= 100 cents). This compiles only
    // when the returned iterator borrows `entries` for `'a`.
    assert_eq!(count_filtered(&entries, &filter), 2);
}
