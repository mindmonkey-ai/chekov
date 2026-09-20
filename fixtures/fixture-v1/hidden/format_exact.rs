// Held-out assertion — device 7 (exact cents formatting). NEVER materialized
// into prompt context; injected by the grader.

use fixture_v1::domain::money::{Cents, format_cents};

#[test]
fn cents_format_exactly_without_float_or_signed_abs_loss() {
    assert_eq!(format_cents(Cents(0)), "0.00");
    assert_eq!(format_cents(Cents(5)), "0.05");
    assert_eq!(format_cents(Cents(-5)), "-0.05");
    assert_eq!(format_cents(Cents(1234)), "12.34");
    assert_eq!(format_cents(Cents(-1234)), "-12.34");
    assert_eq!(
        format_cents(Cents(i128::MIN)),
        "-1701411834604692317316873037158841057.28"
    );
}