// Held-out assertion — device 5 (money conservation: integer-remainder split).
// NEVER materialized into prompt context; injected by the grader.
//
// `split_evenly` is the masked body in `domain/money.rs`. The module doc states
// that splitting money conserves it: the shares must sum back to the exact
// total, with the indivisible remainder cents handed out one each to the
// earliest shares. The tempting `(0..parts).map(|_| total / parts).collect()`
// compiles and is right when the total divides evenly, but drops the remainder
// on every uneven split (100/3 -> [33, 33, 33], losing a cent). This pins the
// conservation.

use fixture_v1::domain::money::split_evenly;

#[test]
fn split_evenly_conserves_the_total_and_hands_out_the_remainder() {
    // 100 cents three ways: [34, 33, 33], summing to exactly 100.
    let shares = split_evenly(100, 3);
    assert_eq!(shares, vec![34, 33, 33]);
    assert_eq!(shares.iter().sum::<i128>(), 100, "money must be conserved");

    // A remainder of 2 goes to the first two shares.
    let shares = split_evenly(11, 3);
    assert_eq!(shares, vec![4, 4, 3]);
    assert_eq!(shares.iter().sum::<i128>(), 11);

    // Conservation also holds below zero: the Euclidean remainder still goes
    // to the earliest shares, and no cent appears or disappears.
    let shares = split_evenly(-100, 3);
    assert_eq!(shares, vec![-33, -33, -34]);
    assert_eq!(shares.iter().sum::<i128>(), -100);

    // Exact division: no remainder to distribute. Zero parts: no shares.
    assert_eq!(split_evenly(9, 3), vec![3, 3, 3]);
    assert_eq!(split_evenly(50, 0), Vec::<i128>::new());
}
