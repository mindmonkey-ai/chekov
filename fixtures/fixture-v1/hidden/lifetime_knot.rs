// Held-out assertion — device 4 (move-capture closure, compile gate). NEVER
// materialized into prompt context; injected by the grader.
//
// `replay_filtered` is the single masked body in `store/replay.rs`; its
// signature is given, so what is graded is the body. The predicate closure has
// to `move`-capture `filter` for the declared `'a`: a closure that borrows
// `filter` from the function frame instead does not live as long as the
// returned iterator and fails to compile — so this task is graded by the
// compile gate (tier 6), not the test gate.

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
