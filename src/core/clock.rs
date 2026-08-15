//! Compact UTC timestamps for backups and license provenance.
//!
//! Hand-rolled civil-from-days conversion (~15 lines) instead of pulling in
//! `chrono`/`time` — the crate budget (§9, prompt §2.1) does not include a
//! date library for two format sites.

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYYMMDDTHHMMSSZ` for a given unix time (pure; unit-tested).
#[must_use]
pub fn utc_compact(secs_since_epoch: u64) -> String {
    let _ = secs_since_epoch;
    todo!("cycle 3 red")
}

/// `utc_compact` for the current instant.
#[must_use]
pub fn utc_compact_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc_compact(secs)
}

#[cfg(test)]
mod tests {
    use super::utc_compact;

    #[test]
    fn formats_epoch_zero() {
        assert_eq!(utc_compact(0), "19700101T000000Z");
    }

    #[test]
    fn formats_known_instant() {
        // 2023-11-14 22:13:20 UTC
        assert_eq!(utc_compact(1_700_000_000), "20231114T221320Z");
    }

    #[test]
    fn handles_leap_year_day() {
        // 2024-02-29 12:00:00 UTC
        assert_eq!(utc_compact(1_709_208_000), "20240229T120000Z");
    }
}
