// Held-out assertion — device 8 (checked money arithmetic). NEVER materialized
// into prompt context; injected by the grader.

use fixture_v1::domain::money::{Cents, CreditCommand, checked_apply};

#[test]
fn checked_apply_preserves_direction_and_reports_overflow() {
    assert_eq!(
        checked_apply(CreditCommand::Credit(Cents(25)), Cents(100)),
        Some(Cents(125))
    );
    assert_eq!(
        checked_apply(CreditCommand::Debit(Cents(25)), Cents(100)),
        Some(Cents(75))
    );
    assert_eq!(
        checked_apply(CreditCommand::Credit(Cents(1)), Cents(i128::MAX)),
        None
    );
    assert_eq!(
        checked_apply(CreditCommand::Debit(Cents(1)), Cents(i128::MIN)),
        None
    );
    assert_eq!(
        checked_apply(CreditCommand::Debit(Cents(i128::MIN)), Cents(0)),
        None
    );
}