// Held-out assertion — device 2 (the near-miss API). NEVER materialized into
// prompt context; injected by the grader and excluded from every prompt by
// construction (see manifest.toml).
//
// `Dispatcher::handle_credit` is the single masked body in `api/`. The obvious
// wrong call is `self.ledger.append_entry(cmd)`: it compiles, returns `Ok`, and
// leaves the projection balance at 0. Only `self.ledger.apply_entry(cmd)` folds
// the command into the running balance. This assertion pins the correct one.

use fixture_v1::api::Dispatcher;
use fixture_v1::domain::money::{CreditCommand, from_str};

#[test]
fn handle_credit_advances_the_running_projection() {
    let mut d = Dispatcher::new();
    d.handle_credit(CreditCommand::Credit(from_str("3000.00").unwrap()));
    // append_entry would leave this at 0; apply_entry advances it to 300000c.
    assert_eq!(d.balance(), 300_000, "handle_credit must fold into the projection");
    d.handle_debit(CreditCommand::Debit(from_str("750.00").unwrap()));
    assert_eq!(d.balance(), 225_000);
}
