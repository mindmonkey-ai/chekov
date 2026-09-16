//! Replay the recorded entries newest-first. Two devices live here:
//!
//! #2 (cross-file first-use mask): `replay_filtered` is the first use of
//! `LedgerEntry`'s fields outside `domain/`, so a model that read only this
//! file cannot know that `LedgerEntry` has a `command`/`outcome` shape. The
//! field accesses are the mask.
//!
//! #4 (generic + lifetime knot): the signature type-checks exactly one way.
//! The returned iterator borrows `entries` for `'a` and is filtered by a
//! closure that captures `&'a Filter`. Getting the lifetime wrong (e.g.
//! borrowing the filter for a shorter scope than the iterator) fails to
//! compile; the one correct form ties the iterator borrow to `entries`.

use crate::domain::ledger::LedgerEntry;

/// A predicate over recorded entries. `Filter` carries the comparison the
/// caller wants; `matches` is the predicate body.
pub struct Filter {
    min_credits_cents: i128,
}

impl Filter {
    /// Keep entries whose credit amount is at least `min_credits_cents`.
    pub fn min_credits(min_credits_cents: i128) -> Self {
        Self { min_credits_cents }
    }

    fn matches(&self, entry: &LedgerEntry) -> bool {
        use crate::domain::ledger::CreditOutcome;
        use crate::domain::money::CreditCommand;
        match (&entry.command, &entry.outcome) {
            (CreditCommand::Credit(c), CreditOutcome::Applied { balance, .. }) => {
                c.0 >= self.min_credits_cents && *balance >= self.min_credits_cents
            }
            _ => false,
        }
    }
}

/// Record entries into a `Ledger` and stream the surviving entries through a
/// `Filter`, newest-first.
///
/// **TASK 4 (lifetime knot).** The signature must tie the iterator borrow to
/// `entries` and the closure borrow to `filter` for the same `'a`:
/// ```text
/// pub fn replay_filtered<'a>(
///     entries: &'a [LedgerEntry],
///     filter: &'a Filter,
/// ) -> impl Iterator<Item = &'a LedgerEntry> + 'a
/// ```
/// A near-miss that returns `impl Iterator` borrowing only `entries` while
/// capturing `filter` for a shorter lifetime fails to compile — the closure
/// outlives the value it borrows.

        // MASKED — TASK 4 (lifetime knot, §9 device 4). The reference body below is
        // the one signature that type-checks. The returned iterator must borrow
        // `entries` and the filter closure must borrow `filter` for the SAME 'a:
        // pub fn replay_filtered<'a>(entries: &'a [LedgerEntry], filter: &'a Filter)
        //     -> impl Iterator<Item = &'a LedgerEntry> + 'a
        // A near-miss that lets the closure capture filter for a shorter lifetime
        // than the returned iterator fails to compile (the closure outlives what it borrows).
pub fn replay_filtered<'a>(
    entries: &'a [LedgerEntry],
    filter: &'a Filter,
) -> impl Iterator<Item = &'a LedgerEntry> + 'a {
    entries.iter().filter(move |e| filter.matches(e))
}

/// Fold the recorded entries through `replay_filtered`, returning the count of
/// surviving entries. A convenience over the iterator form above.
pub fn count_filtered(entries: &[LedgerEntry], filter: &Filter) -> usize {
    replay_filtered(entries, filter).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ledger::{CreditOutcome, LedgerEntry};
    use crate::domain::money::{CreditCommand, from_str};

    fn credit(amount: &str) -> LedgerEntry {
        LedgerEntry {
            command: CreditCommand::Credit(from_str(amount).unwrap()),
            outcome: CreditOutcome::Applied {
                balance: from_str(amount).unwrap().0,
                log_len: 0,
            },
        }
    }

    #[test]
    fn replay_filtered_keeps_entries_above_threshold() {
        let entries = [credit("1.00"), credit("2.00"), credit("0.50")];
        let filter = Filter::min_credits(100);
        // only 1.00 and 2.00 survive the filter (>= 100 cents)
        assert_eq!(count_filtered(&entries, &filter), 2);
        assert_eq!(replay_filtered(&entries, &filter).count(), 2);
    }

    #[test]
    fn the_signature_borrows_entries_and_filter_for_the_same_a() {
        // This test exists to pin the lifetime knot: it compiles only when the
        // returned iterator borrows `entries` for `'a`.
        let ledger = LedgerEntry {
            command: CreditCommand::Credit(from_str("10.00").unwrap()),
            outcome: CreditOutcome::Applied {
                balance: from_str("10.00").unwrap().0,
                log_len: 0,
            },
        };
        let entries: Vec<LedgerEntry> = vec![ledger];
        let filter = Filter::min_credits(100);
        assert_eq!(replay_filtered(&entries, &filter).count(), entries.len());
    }
}
