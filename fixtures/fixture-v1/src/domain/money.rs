//! Money primitives. `Cents` is the one value with an invariant, so it is a
//! newtype over `i128` (spec: newtypes for values with invariants; never
//! construct it from an `f64`).

/// A whole-number currency amount. `i128` cents: never an `f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Cents(pub i128);

/// Parse a decimal currency string into exact `i128` cents.
///
/// This is the only constructor that must be used. `2499.95` is `249995`.
/// Exactly two fractional digits are allowed: a string with a third (`12.345`)
/// is rejected, not truncated, so the parse is total over the accepted format.
pub fn from_str(s: &str) -> Result<Cents, &'static str> {
    let (dollars, frac) = match s.split_once('.') {
        Some((d, f)) => (d, f),
        None => (s, ""),
    };
    if frac.len() > 2 {
        return Err("too many cent digits — accept at most two fractional places");
    }
    let dollars: i128 = dollars.parse().map_err(|_| "bad dollar amount")?;
    let cents: i128 = if frac.is_empty() {
        0
    } else {
        frac[..frac.len().min(2)]
            .parse()
            .map_err(|_| "bad cent amount")?
    };
    Ok(Cents(dollars * 100 + cents))
}

/// Build `Cents` from a whole-number integer dollar amount (the obvious way to
/// make money), kept honest with an explicit constructor name so callers
/// cannot write `(i32 * 100) as i128` and confuse themselves.
pub fn from_whole_dollars(whole: i128) -> Cents {
    Cents(whole * 100)
}

/// A debit or credit command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditCommand {
    Credit(Cents),
    Debit(Cents),
}

/// Apply a single command to a running balance. `Credit` adds, `Debit`
/// subtracts. Pure arithmetic over `i128` cents.
pub fn apply(cmd: CreditCommand, balance: Cents) -> Cents {
    match cmd {
        CreditCommand::Credit(c) => Cents(balance.0 + c.0),
        CreditCommand::Debit(c) => Cents(balance.0 - c.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_parses_whole_dollars() {
        assert_eq!(from_str("100").unwrap(), Cents(10_000));
    }

    #[test]
    fn from_str_is_exact_on_the_invariant_candidate() {
        // 2499.95 -> 249995 cents. A float path gives 249994.
        assert_eq!(from_str("2499.95").unwrap(), Cents(249_995));
    }

    #[test]
    fn from_str_rejects_nonsense() {
        assert!(from_str("12.345").is_err(), "three fractional places reject");
        assert!(from_str("abc").is_err(), "non-numeric rejects");
    }

    #[test]
    fn apply_adds_and_subtracts() {
        let bal = from_whole_dollars(10);
        assert_eq!(apply(CreditCommand::Credit(Cents(250)), bal), Cents(1250));
        assert_eq!(apply(CreditCommand::Debit(Cents(250)), bal), Cents(750));
    }
}
