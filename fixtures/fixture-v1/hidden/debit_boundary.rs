// Held-out assertion — device 6 (overdraft exact boundary). NEVER materialized
// into prompt context; injected by the grader.
//
// `debit_allowed` is the masked body in `domain/ledger.rs`. The rule, stated on
// `LedgerError::InsufficientFunds`, is that a `Debit` is refused only when its
// amount is *greater than* the balance — a `Debit` of exactly the balance is
// allowed and settles to zero. The tempting `c.0 < balance` guard compiles and
// passes an ordinary overdraft test but wrongly refuses the exact-balance debit
// this pins.

use fixture_v1::domain::ledger::debit_allowed;
use fixture_v1::domain::money::{from_str, CreditCommand};

#[test]
fn a_debit_of_exactly_the_balance_is_allowed_but_one_more_cent_is_not() {
    let balance = from_str("100.00").unwrap().0;
    // A debit of exactly the balance is allowed (settles to zero).
    let exact = CreditCommand::Debit(from_str("100.00").unwrap());
    assert!(
        debit_allowed(balance, exact),
        "a debit equal to the balance is allowed"
    );
    // One cent more overdraws and is refused.
    let over = CreditCommand::Debit(from_str("100.01").unwrap());
    assert!(
        !debit_allowed(balance, over),
        "a debit over the balance is refused"
    );
    // A smaller debit is allowed; a credit is always allowed.
    assert!(debit_allowed(
        balance,
        CreditCommand::Debit(from_str("1.00").unwrap())
    ));
    assert!(debit_allowed(
        0,
        CreditCommand::Credit(from_str("5.00").unwrap())
    ));
}
