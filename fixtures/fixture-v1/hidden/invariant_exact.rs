// Held-out assertion — device 3 (invariant trap). NEVER materialized into
// prompt context; injected by the grader.
//
// `from_str` is the single masked body in `domain/money.rs`. The tempting
// alternative `(s.parse::<f64>() * 100.0) as i128` compiles and passes
// `from_str("0.01")` but underflows the cents by one on `2499.95` (249994,
// not 249995). The invariant, stated in `domain/mod.rs`, is that money is
// `i128` cents parsed from the string — never built from an `f64`.

use fixture_v1::domain::money::{Cents, from_str};

#[test]
fn from_str_is_exact_over_str_path_not_float() {
    assert_eq!(from_str("2499.95").unwrap(), Cents(249_995));
    assert_eq!(from_str("0.01").unwrap(), Cents(1));
    assert_eq!(from_str("100").unwrap(), Cents(10_000));
    assert_eq!(from_str("0.10").unwrap(), Cents(10));
}
