//! Compact UTC timestamps for backups and license provenance.
//!
//! Hand-rolled civil-from-days conversion (~15 lines) instead of pulling in
//! `chrono`/`time` — the crate budget (§9, prompt §2.1) does not include a
//! date library for two format sites.

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYYMMDDTHHMMSSZ` for a given unix time (pure; unit-tested).
#[must_use]
pub fn utc_compact(secs_since_epoch: u64) -> String {
    let (h, m, s) = (
        secs_since_epoch / 3600 % 24,
        secs_since_epoch / 60 % 60,
        secs_since_epoch % 60,
    );
    let (year, month, day) = civil_from_days(secs_since_epoch / 86_400);
    format!("{year:04}{month:02}{day:02}T{h:02}{m:02}{s:02}Z")
}

/// Days-since-epoch → (year, month, day), Howard Hinnant's civil algorithm.
const fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
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
